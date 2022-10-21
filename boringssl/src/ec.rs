use crate::{digest_into_openssl, openssl_err, ossl};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::DerefMut;
use kmr_common::{
    crypto,
    crypto::{ec, ec::Key, AccumulatingOperation, CurveType, OpaqueOr},
    explicit, km_err, vec_try, Error, FallibleAllocExt,
};
use kmr_wire::{
    keymint,
    keymint::{Digest, EcCurve},
};
use openssl::hash::MessageDigest;

#[cfg(soong)]
fn private_key_from_der_for_group(
    der: &[u8],
    group: &openssl::ec::EcGroupRef,
) -> Result<openssl::ec::EcKey<openssl::pkey::Private>, openssl::error::ErrorStack> {
    // This method is an Android modification to the rust-openssl crate.
    openssl::ec::EcKey::private_key_from_der_for_group(der, group)
}

#[cfg(not(soong))]
fn private_key_from_der_for_group(
    der: &[u8],
    _group: &openssl::ec::EcGroupRef,
) -> Result<openssl::ec::EcKey<openssl::pkey::Private>, openssl::error::ErrorStack> {
    // This doesn't work if the encoded data is missing the curve.
    openssl::ec::EcKey::private_key_from_der(der)
}

/// [`crypto::Ec`] implementation based on BoringSSL.
pub struct BoringEc;

impl crypto::Ec for BoringEc {
    fn generate_nist_key(
        &self,
        _rng: &mut dyn crypto::Rng,
        curve: ec::NistCurve,
        _params: &[keymint::KeyParam],
    ) -> Result<crypto::KeyMaterial, Error> {
        let ec_key = ossl!(openssl::ec::EcKey::<openssl::pkey::Private>::generate(
            nist_curve_to_group(curve)?.as_ref()
        ))?;
        let nist_key = ec::NistKey(ossl!(ec_key.private_key_to_der())?);
        let key = match curve {
            ec::NistCurve::P224 => Key::P224(nist_key),
            ec::NistCurve::P256 => Key::P256(nist_key),
            ec::NistCurve::P384 => Key::P384(nist_key),
            ec::NistCurve::P521 => Key::P521(nist_key),
        };
        Ok(crypto::KeyMaterial::Ec(curve.into(), CurveType::Nist, key.into()))
    }

    fn generate_ed25519_key(
        &self,
        _rng: &mut dyn crypto::Rng,
        _params: &[keymint::KeyParam],
    ) -> Result<crypto::KeyMaterial, Error> {
        let pkey = ossl!(openssl::pkey::PKey::generate_ed25519())?;
        let key = ossl!(pkey.raw_private_key())?;
        let key: [u8; ec::CURVE25519_PRIV_KEY_LEN] = key.try_into().map_err(|e| {
            km_err!(UnknownError, "generated Ed25519 key of unexpected size: {:?}", e)
        })?;
        let key = Key::Ed25519(ec::Ed25519Key(key));
        Ok(crypto::KeyMaterial::Ec(EcCurve::Curve25519, CurveType::EdDsa, key.into()))
    }

    fn generate_x25519_key(
        &self,
        _rng: &mut dyn crypto::Rng,
        _params: &[keymint::KeyParam],
    ) -> Result<crypto::KeyMaterial, Error> {
        let pkey = ossl!(openssl::pkey::PKey::generate_x25519())?;
        let key = ossl!(pkey.raw_private_key())?;
        let key: [u8; ec::CURVE25519_PRIV_KEY_LEN] = key.try_into().map_err(|e| {
            km_err!(UnknownError, "generated X25519 key of unexpected size: {:?}", e)
        })?;
        let key = Key::X25519(ec::X25519Key(key));
        Ok(crypto::KeyMaterial::Ec(EcCurve::Curve25519, CurveType::Xdh, key.into()))
    }

    fn nist_public_key(&self, key: &ec::NistKey, curve: ec::NistCurve) -> Result<Vec<u8>, Error> {
        let group = nist_curve_to_group(curve)?;
        let ec_key = ossl!(private_key_from_der_for_group(&key.0, group.as_ref()))?;
        let pt = ec_key.public_key();
        let mut bn_ctx = ossl!(openssl::bn::BigNumContext::new())?;
        ossl!(pt.to_bytes(
            group.as_ref(),
            openssl::ec::PointConversionForm::UNCOMPRESSED,
            bn_ctx.deref_mut()
        ))
    }

    fn ed25519_public_key(&self, key: &ec::Ed25519Key) -> Result<Vec<u8>, Error> {
        let pkey = ossl!(openssl::pkey::PKey::private_key_from_raw_bytes(
            &key.0,
            openssl::pkey::Id::ED25519
        ))?;
        ossl!(pkey.raw_public_key())
    }

    fn x25519_public_key(&self, key: &ec::X25519Key) -> Result<Vec<u8>, Error> {
        let pkey = ossl!(openssl::pkey::PKey::private_key_from_raw_bytes(
            &key.0,
            openssl::pkey::Id::X25519
        ))?;
        ossl!(pkey.raw_public_key())
    }

    fn begin_agree(&self, key: OpaqueOr<Key>) -> Result<Box<dyn AccumulatingOperation>, Error> {
        let key = explicit!(key)?;
        // Maximum size for a `SubjectPublicKeyInfo` that holds an EC public key is:
        //
        // 30 LL  SEQUENCE + len (SubjectPublicKeyInfo)
        // 30 LL  SEQUENCE + len (AlgorithmIdentifier)
        // 06 07  OID + len
        //     2a8648ce3d0201  (ecPublicKey OID)
        // 06 08  OID + len
        //     2a8648ce3d030107 (P-256 curve OID, which is the longest)
        // 03 42  BIT STRING + len
        //     00  zero pad bits
        //     04  uncompressed
        //     ...  66 bytes of P-521 X coordinate
        //     ...  66 bytes of P-521 Y coordinate
        //
        // Round up a bit just in case.
        let max_size = 164;
        Ok(Box::new(BoringEcAgreeOperation { key, pending_input: Vec::new(), max_size }))
    }

    fn begin_sign(
        &self,
        key: OpaqueOr<Key>,
        digest: Digest,
    ) -> Result<Box<dyn AccumulatingOperation>, Error> {
        let key = explicit!(key)?;
        let curve = key.curve();
        match key {
            Key::P224(key) | Key::P256(key) | Key::P384(key) | Key::P521(key) => {
                let curve = ec::NistCurve::try_from(curve)?;
                if let Some(digest) = digest_into_openssl(digest) {
                    Ok(Box::new(BoringEcDigestSignOperation::new(key, curve, digest)?))
                } else {
                    Ok(Box::new(BoringEcUndigestSignOperation::new(key, curve)?))
                }
            }
            Key::Ed25519(key) => Ok(Box::new(BoringEd25519SignOperation::new(key)?)),
            Key::X25519(_) => Err(km_err!(UnknownError, "X25519 key not valid for signing")),
        }
    }
}

/// [`crypto::EcAgreeOperation`] based on BoringSSL.
pub struct BoringEcAgreeOperation {
    key: Key,
    pending_input: Vec<u8>, // Limited to `max_size` below.
    // Size of a `SubjectPublicKeyInfo` holding peer public key.
    max_size: usize,
}

impl crypto::AccumulatingOperation for BoringEcAgreeOperation {
    fn max_input_size(&self) -> Option<usize> {
        Some(self.max_size)
    }

    fn update(&mut self, data: &[u8]) -> Result<(), Error> {
        self.pending_input.try_extend_from_slice(data)?;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Vec<u8>, Error> {
        let peer_key = ossl!(openssl::pkey::PKey::public_key_from_der(&self.pending_input))?;
        match &self.key {
            Key::P224(key) | Key::P256(key) | Key::P384(key) | Key::P521(key) => {
                let group = nist_key_to_group(&self.key)?;
                let ec_key = ossl!(private_key_from_der_for_group(&key.0, group.as_ref()))?;
                let pkey = ossl!(openssl::pkey::PKey::from_ec_key(ec_key))?;
                let mut deriver = ossl!(openssl::derive::Deriver::new(&pkey))?;
                ossl!(deriver.set_peer(&peer_key))
                    .map_err(|e| km_err!(InvalidArgument, "peer key invalid: {:?}", e))?;
                let derived = ossl!(deriver.derive_to_vec())?;
                Ok(derived)
            }
            #[cfg(soong)]
            Key::X25519(key) => {
                // The BoringSSL `EVP_PKEY` interface does not support X25519, so need to invoke the
                // `ffi:X25519()` method directly. First need to extract the raw peer key from the
                // `SubjectPublicKeyInfo` it arrives in.
                use bssl_ffi as ffi;

                let peer_key =
                    ossl!(openssl::pkey::PKey::public_key_from_der(&self.pending_input))?;
                let peer_key_type = peer_key.id();
                if peer_key_type != openssl::pkey::Id::X25519 {
                    return Err(km_err!(
                        InvalidArgument,
                        "peer key for {:?} not supported with X25519",
                        peer_key_type
                    ));
                }
                let peer_key_data = ossl!(peer_key.raw_public_key())?;
                if peer_key_data.len() != ffi::X25519_PUBLIC_VALUE_LEN as usize {
                    return Err(km_err!(
                        UnknownError,
                        "peer raw key invalid length {}",
                        peer_key_data.len()
                    ));
                }

                let mut sig = vec_try![0; ffi::X25519_SHARED_KEY_LEN as usize]?;
                let result = unsafe {
                    // Safety: all pointer arguments need to point to 32-byte memory areas, enforced
                    // above and in the definition of [`ec::X25519Key`].
                    ffi::X25519(sig.as_mut_ptr(), &key.0 as *const u8, peer_key_data.as_ptr())
                };
                if result == 1 {
                    Ok(sig)
                } else {
                    Err(super::openssl_last_err())
                }
            }
            #[cfg(not(soong))]
            Key::X25519(_) => Err(km_err!(UnknownError, "X25519 not supported in cargo")),
            Key::Ed25519(_) => Err(km_err!(UnknownError, "Ed25519 key not valid for agreement")),
        }
    }
}

/// [`crypto::EcSignOperation`] based on BoringSSL, when an external digest is used.
pub struct BoringEcDigestSignOperation {
    pkey: openssl::pkey::PKey<openssl::pkey::Private>,
    digest: MessageDigest,
    pending_input: Vec<u8>, // TODO: need size limit
}

impl BoringEcDigestSignOperation {
    fn new(key: ec::NistKey, curve: ec::NistCurve, digest: MessageDigest) -> Result<Self, Error> {
        let group = nist_curve_to_group(curve)?;
        let ec_key = ossl!(private_key_from_der_for_group(&key.0, group.as_ref()))?;
        let pkey = ossl!(openssl::pkey::PKey::from_ec_key(ec_key))?;
        Ok(Self { pkey, digest, pending_input: Vec::new() })
    }
}

impl crypto::AccumulatingOperation for BoringEcDigestSignOperation {
    fn update(&mut self, data: &[u8]) -> Result<(), Error> {
        // TODO: figure out a way to fix Signer lifetime to allow incremental feeding
        self.pending_input.extend_from_slice(data);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Vec<u8>, Error> {
        let mut signer = ossl!(openssl::sign::Signer::new(self.digest, &self.pkey))?;
        ossl!(signer.update(&self.pending_input))?;
        let sig = ossl!(signer.sign_to_vec())?;
        Ok(sig)
    }
}

/// [`crypto::EcSignOperation`] based on BoringSSL, when data is undigested.
pub struct BoringEcUndigestSignOperation {
    ec_key: openssl::ec::EcKey<openssl::pkey::Private>,
    pending_input: Vec<u8>,
    max_size: usize,
}

impl BoringEcUndigestSignOperation {
    fn new(key: ec::NistKey, curve: ec::NistCurve) -> Result<Self, Error> {
        let group = nist_curve_to_group(curve)?;
        let ec_key = ossl!(private_key_from_der_for_group(&key.0, group.as_ref()))?;
        // Input to an undigested EC signing operation must be smaller than key size.
        Ok(Self { ec_key, pending_input: Vec::new(), max_size: curve.coord_len() })
    }
}

impl crypto::AccumulatingOperation for BoringEcUndigestSignOperation {
    fn update(&mut self, data: &[u8]) -> Result<(), Error> {
        // For ECDSA signing, extra data beyond the maximum size is ignored (rather than being
        // rejected via the `max_input_size()` trait method).
        let max_extra_data = self.max_size - self.pending_input.len();
        if max_extra_data > 0 {
            let len = core::cmp::min(max_extra_data, data.len());
            self.pending_input.try_extend_from_slice(&data[..len])?;
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Vec<u8>, Error> {
        // BoringSSL doesn't support `EVP_PKEY` use without digest, so use low-level ECDSA
        // functionality.
        let sig = ossl!(openssl::ecdsa::EcdsaSig::sign(&self.pending_input, &self.ec_key))?;
        let sig = ossl!(sig.to_der())?;
        Ok(sig)
    }
}

/// [`crypto::EcSignOperation`] based on BoringSSL for Ed25519.
pub struct BoringEd25519SignOperation {
    pkey: openssl::pkey::PKey<openssl::pkey::Private>,
    pending_input: Vec<u8>,
}

impl BoringEd25519SignOperation {
    fn new(key: ec::Ed25519Key) -> Result<Self, Error> {
        let pkey = ossl!(openssl::pkey::PKey::private_key_from_raw_bytes(
            &key.0,
            openssl::pkey::Id::ED25519
        ))?;
        Ok(Self { pkey, pending_input: Vec::new() })
    }
}

impl crypto::AccumulatingOperation for BoringEd25519SignOperation {
    fn max_input_size(&self) -> Option<usize> {
        // Ed25519 has an internal digest so could theoretically take arbitrary amounts of
        // data. However, BoringSSL does not support incremental data feeding for Ed25519 so
        // instead impose a message size limit (as required by the KeyMint HAL spec).
        Some(ec::MAX_ED25519_MSG_SIZE)
    }

    fn update(&mut self, data: &[u8]) -> Result<(), Error> {
        // OK to accumulate data as there is a size limit.
        self.pending_input.try_extend_from_slice(data)?;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Vec<u8>, Error> {
        let mut signer = ossl!(openssl::sign::Signer::new_without_digest(&self.pkey))?;
        let sig = ossl!(signer.sign_oneshot_to_vec(&self.pending_input))?;
        Ok(sig)
    }
}

fn nist_curve_to_group(curve: ec::NistCurve) -> Result<openssl::ec::EcGroup, Error> {
    use openssl::nid::Nid;
    openssl::ec::EcGroup::from_curve_name(match curve {
        ec::NistCurve::P224 => Nid::SECP224R1,
        ec::NistCurve::P256 => Nid::X9_62_PRIME256V1,
        ec::NistCurve::P384 => Nid::SECP384R1,
        ec::NistCurve::P521 => Nid::SECP521R1,
    })
    .map_err(openssl_err!("failed to determine EcGroup"))
}

fn nist_key_to_group(key: &ec::Key) -> Result<openssl::ec::EcGroup, Error> {
    use openssl::nid::Nid;
    openssl::ec::EcGroup::from_curve_name(match key {
        ec::Key::P224(_) => Nid::SECP224R1,
        ec::Key::P256(_) => Nid::X9_62_PRIME256V1,
        ec::Key::P384(_) => Nid::SECP384R1,
        ec::Key::P521(_) => Nid::SECP521R1,
        ec::Key::Ed25519(_) | ec::Key::X25519(_) => {
            return Err(km_err!(UnknownError, "no NIST group for curve25519 key"))
        }
    })
    .map_err(openssl_err!("failed to determine EcGroup"))
}
