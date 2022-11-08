//! TA functionality related to key generation/import/upgrade.

use crate::{cert, device};
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::{borrow::Borrow, convert::TryFrom};
use der::{Decode, Sequence};
use kmr_common::{
    crypto::{self, aes, rsa, KeyMaterial, OpaqueOr},
    get_bool_tag_value, get_opt_tag_value, get_tag_value, keyblob, km_err, tag, try_to_vec,
    vec_try_with_capacity, Error, FallibleAllocExt,
};
use kmr_wire::{
    keymint::{
        AttestationKey, Digest, EcCurve, ErrorCode, HardwareAuthenticatorType, KeyCharacteristics,
        KeyCreationResult, KeyFormat, KeyOrigin, KeyParam, KeyPurpose, SecurityLevel,
    },
    *,
};
use log::{error, warn};
use spki::SubjectPublicKeyInfo;
use x509_cert::ext::pkix::KeyUsages;

/// Contents of wrapping key data
///
/// ```asn1
/// SecureKeyWrapper ::= SEQUENCE {
///     version                   INTEGER, # Value 0
///     encryptedTransportKey     OCTET_STRING,
///     initializationVector      OCTET_STRING,
///     keyDescription            KeyDescription, # See below
///     encryptedKey              OCTET_STRING,
///     tag                       OCTET_STRING,
/// }
/// ```
#[derive(Debug, Clone, Sequence)]
pub struct SecureKeyWrapper<'a> {
    pub version: i32,
    #[asn1(type = "OCTET STRING")]
    pub encrypted_transport_key: &'a [u8],
    #[asn1(type = "OCTET STRING")]
    pub initialization_vector: &'a [u8],
    pub key_description: KeyDescription,
    #[asn1(type = "OCTET STRING")]
    pub encrypted_key: &'a [u8],
    #[asn1(type = "OCTET STRING")]
    pub tag: &'a [u8],
}

const SECURE_KEY_WRAPPER_VERSION: i32 = 0;

/// Contents of key description.
///
/// ``asn1
/// KeyDescription ::= SEQUENCE {
///     keyFormat    INTEGER, # Values from KeyFormat enum
///     keyParams    AuthorizationList, # See cert.rs
/// }
/// ```
#[derive(Debug, Clone, Sequence)]
pub struct KeyDescription {
    pub key_format: i32,
    pub key_params: cert::AuthorizationList,
}

/// Indication of whether key import has a secure wrapper.
#[derive(Debug, Clone, Copy)]
pub(crate) enum KeyImport {
    Wrapped,
    NonWrapped,
}

/// Combined information needed for signing a fresh public key.
#[derive(Clone)]
pub(crate) struct SigningInfo<'a> {
    pub attestation_info: Option<(&'a [u8], &'a [u8])>, // (challenge, app_id)
    pub signing_key: KeyMaterial,
    /// ASN.1 DER encoding of subject field from first cert.
    pub issuer_subject: Vec<u8>,
    /// Cert chain starting with public key for `signing_key`.
    pub chain: Vec<keymint::Certificate>,
}

impl<'a> crate::KeyMintTa<'a> {
    /// Retrieve the signing information.
    pub(crate) fn get_signing_info(
        &self,
        key_type: device::SigningKey,
    ) -> Result<SigningInfo<'a>, Error> {
        let (chain, issuer) = match key_type {
            device::SigningKey::Batch => (&self.batch_chain, &self.batch_issuer),
            device::SigningKey::DeviceUnique => {
                (&self.device_unique_chain, &self.device_unique_issuer)
            }
        };
        if chain.borrow().is_none() {
            // Retrieve and store the cert chain information (as this is public).
            let dev_chain = self.dev.sign_info.cert_chain(key_type)?;
            let issuer_data = cert::extract_subject(
                dev_chain.get(0).ok_or_else(|| km_err!(UnknownError, "empty attestation chain"))?,
            )?;
            *chain.borrow_mut() = Some(dev_chain);
            *issuer.borrow_mut() = Some(issuer_data);
        }
        // Retrieve the signing key information (which will be dropped when signing is done).
        let signing_key = self.dev.sign_info.signing_key(key_type)?;
        Ok(SigningInfo {
            attestation_info: None,
            signing_key,
            issuer_subject: issuer
                .borrow()
                .as_ref()
                .ok_or_else(|| {
                    km_err!(AttestationKeysNotProvisioned, "no attestation chain available")
                })?
                .clone(),
            chain: chain
                .borrow()
                .as_ref()
                .ok_or_else(|| {
                    km_err!(AttestationKeysNotProvisioned, "no attestation chain available")
                })?
                .clone(),
        })
    }

    /// Generate an X.509 leaf certificate.
    pub(crate) fn generate_cert(
        &self,
        info: Option<SigningInfo>,
        spki: SubjectPublicKeyInfo,
        params: &[KeyParam],
        chars: &[KeyCharacteristics],
    ) -> Result<keymint::Certificate, Error> {
        // Build and encode key usage extension value
        let key_usage_ext_bits = cert::key_usage_extension_bits(params);
        let key_usage_ext_val = cert::asn1_der_encode(&key_usage_ext_bits)?;

        // Build and encode basic constraints extension value, based on the key usage extension
        // value
        let basic_constraints_ext_val =
            if (key_usage_ext_bits.0 & KeyUsages::KeyCertSign).bits().count_ones() != 0 {
                let basic_constraints = cert::basic_constraints_ext_value(true);
                Some(cert::asn1_der_encode(&basic_constraints)?)
            } else {
                None
            };

        // Build and encode attestation extension if present
        let id_info = self.get_attestation_ids();
        let attest_ext_val =
            if let Some(SigningInfo { attestation_info: Some((challenge, app_id)), .. }) = &info {
                let unique_id = self.calculate_unique_id(app_id, params)?;
                let attest_ext = cert::attestation_extension(
                    challenge,
                    app_id,
                    self.hw_info.security_level,
                    id_info.as_ref().map(|v| v.borrow()),
                    params,
                    chars,
                    &unique_id,
                    self.boot_info.as_ref().ok_or_else(|| {
                        km_err!(HardwareNotYetAvailable, "root of trust info not found")
                    })?,
                )?;
                Some(cert::asn1_der_encode(&attest_ext)?)
            } else {
                None
            };

        let tbs_cert = cert::tbs_certificate(
            &info,
            spki,
            &key_usage_ext_val,
            basic_constraints_ext_val.as_deref(),
            attest_ext_val.as_deref(),
            params,
        )?;
        let tbs_data = cert::asn1_der_encode(&tbs_cert)?;
        // If key does not have ATTEST_KEY or SIGN purpose, the certificate has empty signature
        let sig_data = match info.as_ref() {
            Some(info) => self.sign_cert_data(info.signing_key.clone(), tbs_data.as_slice())?,
            None => Vec::new(),
        };

        let cert = cert::certificate(tbs_cert, &sig_data)?;
        let cert_data = cert::asn1_der_encode(&cert)?;
        Ok(keymint::Certificate { encoded_certificate: cert_data })
    }

    /// Perform a complete signing operation using default modes.
    fn sign_cert_data(&self, signing_key: KeyMaterial, tbs_data: &[u8]) -> Result<Vec<u8>, Error> {
        match signing_key {
            KeyMaterial::Rsa(key) => {
                let mut op = self
                    .imp
                    .rsa
                    .begin_sign(key, rsa::SignMode::Pkcs1_1_5Padding(Digest::Sha256))?;
                op.update(tbs_data)?;
                op.finish()
            }
            KeyMaterial::Ec(curve, _, key) => {
                let digest = if curve == EcCurve::Curve25519 {
                    // Ed25519 includes an internal digest and so does not use an external digest.
                    Digest::None
                } else {
                    Digest::Sha256
                };
                let mut op = self.imp.ec.begin_sign(key, digest)?;
                op.update(tbs_data)?;
                op.finish()
            }
            _ => Err(km_err!(UnknownError, "unexpected cert signing key type")),
        }
    }

    /// Calculate the `UNIQUE_ID` value for the parameters, if needed.
    fn calculate_unique_id(&self, app_id: &[u8], params: &[KeyParam]) -> Result<Vec<u8>, Error> {
        if !get_bool_tag_value!(params, IncludeUniqueId)? {
            return Ok(Vec::new());
        }
        let creation_datetime =
            get_tag_value!(params, CreationDatetime, ErrorCode::InvalidArgument)?;
        let rounded_datetime = creation_datetime.ms_since_epoch / 2_592_000_000i64;
        let datetime_data = rounded_datetime.to_ne_bytes();

        let mut combined_input = vec_try_with_capacity!(datetime_data.len() + app_id.len() + 1)?;
        combined_input.extend_from_slice(&datetime_data[..]);
        combined_input.extend_from_slice(app_id);
        combined_input.push(if get_bool_tag_value!(params, ResetSinceIdRotation)? { 1 } else { 0 });

        let hbk = self.dev.keys.unique_id_hbk(Some(self.imp.ckdf))?;

        let mut hmac_op = self.imp.hmac.begin(hbk.into(), Digest::Sha256)?;
        hmac_op.update(&combined_input)?;
        let tag = hmac_op.finish()?;
        try_to_vec(&tag[..16])
    }

    pub(crate) fn generate_key(
        &mut self,
        params: &[KeyParam],
        attestation_key: Option<AttestationKey>,
    ) -> Result<KeyCreationResult, Error> {
        let (mut chars, keygen_info) = tag::extract_key_gen_characteristics(
            self.secure_storage_available(),
            params,
            self.hw_info.security_level,
        )?;
        self.add_keymint_tags(&mut chars, KeyOrigin::Generated)?;
        let key_material = match keygen_info {
            crypto::KeyGenInfo::Aes(variant) => {
                self.imp.aes.generate_key(&mut *self.imp.rng, variant, params)?
            }
            crypto::KeyGenInfo::TripleDes => {
                self.imp.des.generate_key(&mut *self.imp.rng, params)?
            }
            crypto::KeyGenInfo::Hmac(key_size) => {
                self.imp.hmac.generate_key(&mut *self.imp.rng, key_size, params)?
            }
            crypto::KeyGenInfo::Rsa(key_size, pub_exponent) => {
                self.imp.rsa.generate_key(&mut *self.imp.rng, key_size, pub_exponent, params)?
            }
            crypto::KeyGenInfo::NistEc(curve) => {
                self.imp.ec.generate_nist_key(&mut *self.imp.rng, curve, params)?
            }
            crypto::KeyGenInfo::Ed25519 => {
                self.imp.ec.generate_ed25519_key(&mut *self.imp.rng, params)?
            }
            crypto::KeyGenInfo::X25519 => {
                self.imp.ec.generate_x25519_key(&mut *self.imp.rng, params)?
            }
        };

        self.finish_keyblob_creation(params, attestation_key, chars, key_material)
    }

    pub(crate) fn import_key(
        &mut self,
        params: &[KeyParam],
        key_format: KeyFormat,
        key_data: &[u8],
        attestation_key: Option<AttestationKey>,
        import_type: KeyImport,
    ) -> Result<KeyCreationResult, Error> {
        let (mut chars, key_material) = tag::extract_key_import_characteristics(
            &self.imp,
            self.secure_storage_available(),
            params,
            self.hw_info.security_level,
            key_format,
            key_data,
        )?;
        match import_type {
            KeyImport::NonWrapped => {
                self.add_keymint_tags(&mut chars, KeyOrigin::Imported)?;
            }
            KeyImport::Wrapped => {
                self.add_keymint_tags(&mut chars, KeyOrigin::SecurelyImported)?;
            }
        }

        self.finish_keyblob_creation(params, attestation_key, chars, key_material)
    }

    /// Perform common processing for keyblob creation (for both generation and import).
    fn finish_keyblob_creation(
        &mut self,
        params: &[KeyParam],
        attestation_key: Option<AttestationKey>,
        chars: Vec<KeyCharacteristics>,
        key_material: KeyMaterial,
    ) -> Result<KeyCreationResult, Error> {
        let keyblob = keyblob::PlaintextKeyBlob {
            // Don't include any `SecurityLevel::Keystore` characteristics in the set that is bound
            // to the key.
            characteristics: chars
                .iter()
                .filter(|c| c.security_level != SecurityLevel::Keystore)
                .cloned()
                .collect(),
            key_material: key_material.clone(),
        };
        let attest_keyblob;
        let mut certificate_chain = Vec::new();
        if let Some(spki) =
            keyblob.key_material.subject_public_key_info(&mut Vec::<u8>::new(), self.imp.ec)?
        {
            // Asymmetric keys return the public key inside an X.509 certificate.
            // Need to determine:
            // - a key to sign the cert with (may be absent), together with any associated
            //   cert chain to append
            // - whether to include an attestation extension
            let attest_challenge = get_opt_tag_value!(params, AttestationChallenge)?;

            let signing_info = if let Some(attest_challenge) = attest_challenge {
                // Attestation requested.
                let attest_app_id = get_opt_tag_value!(params, AttestationApplicationId)?
                    .ok_or_else(|| {
                        km_err!(AttestationApplicationIdMissing, "attestation requested")
                    })?;
                let attestation_info: Option<(&[u8], &[u8])> =
                    Some((attest_challenge, attest_app_id));

                if let Some(attest_keyinfo) = attestation_key.as_ref() {
                    // User-specified attestation key provided.
                    let encrypted_attest_keyblob =
                        keyblob::EncryptedKeyBlob::new(&attest_keyinfo.key_blob)?;
                    let attest_hidden =
                        tag::hidden(&attest_keyinfo.attest_key_params, self.root_of_trust()?)?;

                    attest_keyblob =
                        self.keyblob_decrypt(encrypted_attest_keyblob, attest_hidden)?;
                    attest_keyblob
                        .suitable_for(KeyPurpose::AttestKey, self.hw_info.security_level)?;
                    if attest_keyinfo.issuer_subject_name.is_empty() {
                        return Err(km_err!(InvalidArgument, "empty subject name"));
                    }
                    Some(SigningInfo {
                        attestation_info,
                        signing_key: attest_keyblob.key_material,
                        issuer_subject: attest_keyinfo.issuer_subject_name.clone(),
                        chain: Vec::new(),
                    })
                } else {
                    // Need to use a device key for attestation. Look up the relevant device key and
                    // chain.
                    let key_type = match (
                        get_bool_tag_value!(params, DeviceUniqueAttestation)?,
                        self.is_strongbox(),
                    ) {
                        (false, _) => device::SigningKey::Batch,
                        (true, true) => device::SigningKey::DeviceUnique,
                        (true, false) => {
                            return Err(km_err!(
                                InvalidArgument,
                                "device unique attestation supported only by Strongbox TA"
                            ))
                        }
                    };

                    let mut info = self.get_signing_info(key_type)?;
                    info.attestation_info = attestation_info;
                    Some(info)
                }
            } else {
                // No attestation challenge, so no attestation.
                if attestation_key.is_some() {
                    return Err(km_err!(
                        AttestationChallengeMissing,
                        "got attestation key but no challenge"
                    ));
                }

                // See if the generated key can self-sign.
                let is_signing_key = params.iter().any(|param| {
                    matches!(
                        param,
                        KeyParam::Purpose(KeyPurpose::Sign)
                            | KeyParam::Purpose(KeyPurpose::AttestKey)
                    )
                });
                if is_signing_key {
                    Some(SigningInfo {
                        attestation_info: None,
                        signing_key: key_material,
                        issuer_subject: try_to_vec(tag::get_cert_subject(params)?)?,
                        chain: Vec::new(),
                    })
                } else {
                    None
                }
            };

            // Build the X.509 leaf certificate.
            let leaf_cert = self.generate_cert(signing_info.clone(), spki, params, &chars)?;
            certificate_chain.try_push(leaf_cert)?;

            // Append the rest of the chain.
            if let Some(info) = signing_info {
                for cert in info.chain {
                    certificate_chain.try_push(cert)?;
                }
            }
        }

        // Now build the keyblob.
        let root_kek = self.root_kek();
        let hidden = tag::hidden(params, self.root_of_trust()?)?;
        let encrypted_keyblob = keyblob::encrypt(
            self.hw_info.security_level,
            match &mut self.dev.sdd_mgr {
                None => None,
                Some(mr) => Some(*mr),
            },
            self.imp.aes,
            self.imp.hkdf,
            &mut *self.imp.rng,
            &root_kek,
            keyblob,
            hidden,
        )?;
        let serialized_keyblob = encrypted_keyblob.into_vec()?;

        Ok(KeyCreationResult {
            key_blob: serialized_keyblob,
            key_characteristics: chars,
            certificate_chain,
        })
    }

    pub(crate) fn import_wrapped_key(
        &mut self,
        wrapped_key_data: &[u8],
        wrapping_key_blob: &[u8],
        masking_key: &[u8],
        unwrapping_params: &[KeyParam],
        password_sid: i64,
        biometric_sid: i64,
    ) -> Result<KeyCreationResult, Error> {

        // Decrypt the wrapping key blob
        let encrypted_wrapping_key_blob = keyblob::EncryptedKeyBlob::new(wrapping_key_blob)?;
        let hidden_params = tag::hidden(unwrapping_params, self.root_of_trust()?)?;
        let wrapping_key = self.keyblob_decrypt(encrypted_wrapping_key_blob, hidden_params)?;
        let keyblob::PlaintextKeyBlob { characteristics, key_material } = wrapping_key;

        // Decode the ASN.1 DER encoded `SecureKeyWrapper`.
        let secure_key_wrapper = SecureKeyWrapper::from_der(wrapped_key_data)?;

        if secure_key_wrapper.version != SECURE_KEY_WRAPPER_VERSION {
            return Err(km_err!(InvalidArgument, "invalid version in Secure Key Wrapper."));
        }

        // Decrypt the masked transport key.
        let masked_transport_key = match key_material {
            KeyMaterial::Rsa(key) => {
                // Check the requirements on the wrapping key characterisitcs
                let decrypt_mode = tag::check_rsa_wrapping_key_params(
                    keyblob::characteristics_at(&characteristics, self.hw_info.security_level)?,
                    unwrapping_params,
                )?;

                // Decrypt the masked and encrypted transport key
                let mut crypto_op = self.imp.rsa.begin_decrypt(key, decrypt_mode)?;
                crypto_op.as_mut().update(secure_key_wrapper.encrypted_transport_key)?;
                crypto_op.finish()?
            }
            // TODO: For now, we consider wrapping keys to be RSA keys only.
            _ => {
                return Err(km_err!(InvalidArgument, "invalid key algorithm for transport key"));
            }
        };

        if masked_transport_key.len() != masking_key.len() {
            return Err(km_err!(
                InvalidArgument,
                "masked transport key is {} bytes, but masking key is {} bytes",
                masked_transport_key.len(),
                masking_key.len()
            ));
        }

        let unmasked_transport_key: Vec<u8> =
            masked_transport_key.iter().zip(masking_key).map(|(x, y)| x ^ y).collect();

        let aes_transport_key = aes::Key::Aes256(unmasked_transport_key.try_into().map_err(|_e| {
                km_err!(
                    InvalidArgument,
                    "transport key len {} not correct for AES-256 key",
                    masked_transport_key.len()
                )
            })?);

        // Validate the size of the IV and match the `aes::GcmMode` based on the tag size.
        let iv_len = secure_key_wrapper.initialization_vector.len();
        if iv_len != aes::GCM_NONCE_SIZE {
            return Err(km_err!(
                InvalidArgument,
                "IV length is of {} bytes, which should be of {} bytes",
                iv_len,
                aes::GCM_NONCE_SIZE
            ));
        }
        let tag_len = secure_key_wrapper.tag.len();
        let gcm_mode = match tag_len {
            12 => crypto::aes::GcmMode::GcmTag12 {
                nonce: secure_key_wrapper.initialization_vector.try_into()
                .unwrap(/* safe: len checked */),
            },
            13 => crypto::aes::GcmMode::GcmTag13 {
                nonce: secure_key_wrapper.initialization_vector.try_into()
                .unwrap(/* safe: len checked */),
            },
            14 => crypto::aes::GcmMode::GcmTag14 {
                nonce: secure_key_wrapper.initialization_vector.try_into()
                .unwrap(/* safe: len checked */),
            },
            15 => crypto::aes::GcmMode::GcmTag15 {
                nonce: secure_key_wrapper.initialization_vector.try_into()
                .unwrap(/* safe: len checked */),
            },
            16 => crypto::aes::GcmMode::GcmTag16 {
                nonce: secure_key_wrapper.initialization_vector.try_into()
                .unwrap(/* safe: len checked */),
            },
            v => {
                return Err(km_err!(
                    InvalidMacLength,
                    "want 12-16 byte tag for AES-GCM not {} bytes",
                    v
                ))
            }
        };

        // Decrypt the encrypted key to be imported, using the ASN.1 DER (re-)encoding of the key
        // description as the AAD.
        let mut op = self.imp.aes.begin_aead(
            OpaqueOr::Explicit(aes_transport_key),
            gcm_mode,
            crypto::SymmetricOperation::Decrypt,
        )?;
        op.update_aad(&cert::asn1_der_encode(&secure_key_wrapper.key_description)?)?;

        let mut imported_key_data = op.update(secure_key_wrapper.encrypted_key)?;
        imported_key_data.try_extend_from_slice(&op.update(secure_key_wrapper.tag)?)?;
        imported_key_data.try_extend_from_slice(&op.finish()?)?;

        let mut imported_key_params = secure_key_wrapper.key_description.key_params.auths;
        if let Some(secure_id) = get_opt_tag_value!(imported_key_params.clone(), UserSecureId)? {
            // If both the Password and Fingerprint bits are set in UserSecureId, the password SID
            // should be used, because biometric auth tokens contain both password and fingerprint
            // SIDs, but password auth tokens only contain the password SID.
            if (secure_id & (HardwareAuthenticatorType::Password as u64)
                == (HardwareAuthenticatorType::Password as u64))
                && (secure_id & (HardwareAuthenticatorType::Fingerprint as u64)
                    == (HardwareAuthenticatorType::Fingerprint as u64))
            {
                imported_key_params
                    .retain(|key_param| !matches!(key_param, KeyParam::UserSecureId(_)));
                imported_key_params.try_push(KeyParam::UserSecureId(password_sid as u64))?;
            } else if secure_id & (HardwareAuthenticatorType::Password as u64)
                == (HardwareAuthenticatorType::Password as u64)
            {
                imported_key_params
                    .retain(|key_param| !matches!(key_param, KeyParam::UserSecureId(_)));
                imported_key_params.try_push(KeyParam::UserSecureId(password_sid as u64))?;
            } else if secure_id & (HardwareAuthenticatorType::Fingerprint as u64)
                == (HardwareAuthenticatorType::Fingerprint as u64)
            {
                imported_key_params
                    .retain(|key_param| !matches!(key_param, KeyParam::UserSecureId(_)));
                imported_key_params.try_push(KeyParam::UserSecureId(biometric_sid as u64))?;
            }
        };
        self.import_key(
            &imported_key_params,
            KeyFormat::try_from(secure_key_wrapper.key_description.key_format).map_err(|_e| {
                km_err!(
                    UnsupportedKeyFormat,
                    "could not convert the provided keyformat {}",
                    secure_key_wrapper.key_description.key_format
                )
            })?,
            &imported_key_data,
            None,
            KeyImport::Wrapped,
        )
    }

    pub(crate) fn upgrade_key(
        &mut self,
        keyblob_to_upgrade: &[u8],
        upgrade_params: Vec<KeyParam>,
    ) -> Result<Vec<u8>, Error> {
        // TODO: cope with previous versions/encodings of keys
        let encrypted_keyblob = keyblob::EncryptedKeyBlob::new(keyblob_to_upgrade)?;
        let sdd_slot = match &encrypted_keyblob {
            keyblob::EncryptedKeyBlob::V1(blob) => blob.secure_deletion_slot,
        };

        let hidden = tag::hidden(&upgrade_params, self.root_of_trust()?)?;
        let mut keyblob = self.keyblob_decrypt(encrypted_keyblob, hidden.clone())?;

        fn upgrade(v: &mut u32, curr: u32, name: &str) -> Result<bool, Error> {
            match (*v).cmp(&curr) {
                Ordering::Less => {
                    *v = curr;
                    Ok(true)
                }
                Ordering::Equal => Ok(false),
                Ordering::Greater => {
                    error!("refusing to downgrade {} from {} to {}", name, v, curr);
                    Err(km_err!(
                        InvalidArgument,
                        "keyblob with future {} {} (current {})",
                        name,
                        v,
                        curr
                    ))
                }
            }
        }

        let mut modified = false;
        for chars in &mut keyblob.characteristics {
            if chars.security_level != self.hw_info.security_level {
                continue;
            }
            for param in &mut chars.authorizations {
                match param {
                    KeyParam::OsVersion(v) => {
                        if let Some(hal_info) = &self.hal_info {
                            if hal_info.os_version == 0 {
                                // Special case: upgrades to OS version zero are always allowed.
                                warn!("forcing upgrade to OS version 0");
                                modified |= *v != 0;
                                *v = 0;
                            } else {
                                modified |= upgrade(v, hal_info.os_version, "OS version")?;
                            }
                        } else {
                            error!("OS version not available, can't upgrade from {}", v);
                        }
                    }
                    KeyParam::OsPatchlevel(v) => {
                        if let Some(hal_info) = &self.hal_info {
                            modified |= upgrade(v, hal_info.os_patchlevel, "OS patchlevel")?;
                        } else {
                            error!("OS patchlevel not available, can't upgrade from {}", v);
                        }
                    }
                    KeyParam::VendorPatchlevel(v) => {
                        if let Some(hal_info) = &self.hal_info {
                            modified |=
                                upgrade(v, hal_info.vendor_patchlevel, "vendor patchlevel")?;
                        } else {
                            error!("vendor patchlevel not available, can't upgrade from {}", v);
                        }
                    }
                    KeyParam::BootPatchlevel(v) => {
                        if let Some(boot_info) = &self.boot_info {
                            modified |= upgrade(v, boot_info.boot_patchlevel, "boot patchlevel")?;
                        } else {
                            error!("boot patchlevel not available, can't upgrade from {}", v);
                        }
                    }
                    _ => {}
                }
            }
        }

        if !modified {
            // No upgrade needed, return empty data to indicate existing keyblob can still be used.
            return Ok(Vec::new());
        }

        // Need a new keyblob, so free up any existing secure deletion slot. (Re-encryption below
        // will use a new slot and secret.)
        match (sdd_slot, &mut self.dev.sdd_mgr) {
            (Some(slot), Some(mgr)) => {
                if let Err(e) = mgr.delete_secret(slot) {
                    error!(
                        "failed to delete secret for slot {:?} ({:?}): potential leak of slot!",
                        slot, e
                    );
                }
            }
            (Some(slot), None) => {
                return Err(km_err!(
                    UnknownError,
                    "keyblob has sdd slot {:?} but no secure storage available!",
                    slot
                ));
            }
            (None, _) => {}
        }

        // Now re-build the keyblob.
        let root_kek = self.root_kek();
        let encrypted_keyblob = keyblob::encrypt(
            self.hw_info.security_level,
            match &mut self.dev.sdd_mgr {
                None => None,
                Some(mr) => Some(*mr),
            },
            self.imp.aes,
            self.imp.hkdf,
            &mut *self.imp.rng,
            &root_kek,
            keyblob,
            hidden,
        )?;
        Ok(encrypted_keyblob.into_vec()?)
    }
}
