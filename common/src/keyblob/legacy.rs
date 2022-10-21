//! Utilities for handling legacy KeyMaster/KeyMint key blobs.

use crate::tag::legacy::{consume_u32, consume_u8, consume_vec};
use crate::{
    crypto, get_opt_tag_value, km_err, try_to_vec, vec_try_with_capacity, Error, FallibleAllocExt,
};
use alloc::vec::Vec;
use core::mem::size_of;
use kmr_wire::keymint::KeyParam;

#[cfg(test)]
mod tests;

/// Key blob version.
const KEY_BLOB_VERSION: u8 = 0;

/// Hard-coded HMAC key used for keyblob authentication.
const HMAC_KEY: &[u8] = b"IntegrityAssuredBlob0\0";

/// Format of encrypted key blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthEncryptedBlobFormat {
    AesOcb = 0,
    AesGcmWithSwEnforced = 1,
    AesGcmWithSecureDeletion = 2,
}

/// Encrypted key blob, including key characteristics.
#[derive(Debug, PartialEq, Eq)]
pub struct EncryptedKeyBlob {
    pub format: AuthEncryptedBlobFormat,
    // IV for encryption.
    pub nonce: Vec<u8>,
    // Encrypted key material.
    pub ciphertext: Vec<u8>,
    // Authenticated encryption tag.
    pub tag: Vec<u8>,
    pub hw_enforced: Vec<KeyParam>,
    pub sw_enforced: Vec<KeyParam>,
    pub key_slot: Option<u32>,
}

// TODO: add decryption support so that keyblobs from a previous version of KeyMint can be
// upgraded (assuming that the KEK material is available).

impl EncryptedKeyBlob {
    /// Serialize an [`EncryptedKeyBlob`].
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        let hw_enforced_data = crate::tag::legacy::serialize(&self.hw_enforced)?;
        let sw_enforced_data = crate::tag::legacy::serialize(&self.sw_enforced)?;
        let mut result = vec_try_with_capacity!(
            size_of::<u8>()
                + size_of::<u32>()
                + self.nonce.len()
                + size_of::<u32>()
                + self.ciphertext.len()
                + size_of::<u32>()
                + self.tag.len()
                + hw_enforced_data.len()
                + sw_enforced_data.len()
                + size_of::<u32>()
        )?;
        result.push(self.format as u8);
        result.extend_from_slice(&(self.nonce.len() as u32).to_ne_bytes());
        result.extend_from_slice(&self.nonce);
        result.extend_from_slice(&(self.ciphertext.len() as u32).to_ne_bytes());
        result.extend_from_slice(&self.ciphertext);
        result.extend_from_slice(&(self.tag.len() as u32).to_ne_bytes());
        result.extend_from_slice(&self.tag);
        result.extend_from_slice(&hw_enforced_data);
        result.extend_from_slice(&sw_enforced_data);
        if let Some(slot) = self.key_slot {
            result.extend_from_slice(&(slot as u32).to_ne_bytes());
        }
        Ok(result)
    }

    /// Parse a serialized [`KeyBlob`].
    pub fn deserialize(mut data: &[u8]) -> Result<Self, Error> {
        let format = match consume_u8(&mut data)? {
            x if x == AuthEncryptedBlobFormat::AesOcb as u8 => AuthEncryptedBlobFormat::AesOcb,
            x if x == AuthEncryptedBlobFormat::AesGcmWithSwEnforced as u8 => {
                AuthEncryptedBlobFormat::AesGcmWithSwEnforced
            }
            x if x == AuthEncryptedBlobFormat::AesGcmWithSecureDeletion as u8 => {
                AuthEncryptedBlobFormat::AesGcmWithSecureDeletion
            }
            x => return Err(km_err!(InvalidKeyBlob, "unexpected blob format {}", x)),
        };

        let nonce = consume_vec(&mut data)?;
        let ciphertext = consume_vec(&mut data)?;
        let tag = consume_vec(&mut data)?;
        let hw_enforced = crate::tag::legacy::deserialize(&mut data)?;
        let sw_enforced = crate::tag::legacy::deserialize(&mut data)?;

        let key_slot = match data.len() {
            0 => None,
            4 => Some(consume_u32(&mut data)?),
            _ => return Err(km_err!(InvalidKeyBlob, "unexpected remaining length {}", data.len())),
        };

        Ok(EncryptedKeyBlob { format, nonce, ciphertext, tag, hw_enforced, sw_enforced, key_slot })
    }
}

/// Plaintext key blob, with key characteristics.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyBlob {
    pub key_material: Vec<u8>,
    pub hw_enforced: Vec<KeyParam>,
    pub sw_enforced: Vec<KeyParam>,
}

impl KeyBlob {
    /// Size (in bytes) of appended MAC.
    pub const MAC_LEN: usize = 8;

    /// Serialize a [`KeyBlob`].
    pub fn serialize<H: crypto::Hmac>(
        &self,
        hmac: &H,
        hidden: &[KeyParam],
    ) -> Result<Vec<u8>, crate::Error> {
        let hw_enforced_data = crate::tag::legacy::serialize(&self.hw_enforced)?;
        let sw_enforced_data = crate::tag::legacy::serialize(&self.sw_enforced)?;
        let mut result = vec_try_with_capacity!(
            size_of::<u8>()
                + size_of::<u32>()
                + self.key_material.len()
                + hw_enforced_data.len()
                + sw_enforced_data.len()
        )?;
        result.push(KEY_BLOB_VERSION);
        result.extend_from_slice(&(self.key_material.len() as u32).to_ne_bytes());
        result.extend_from_slice(&self.key_material);
        result.extend_from_slice(&hw_enforced_data);
        result.extend_from_slice(&sw_enforced_data);
        let mac = Self::compute_hmac(hmac, &result, hidden)?;
        result.extend_from_slice(&mac);
        Ok(result)
    }

    /// Parse a serialized [`KeyBlob`].
    pub fn deserialize<E: crypto::ConstTimeEq, H: crypto::Hmac>(
        hmac: &H,
        mut data: &[u8],
        hidden: &[KeyParam],
        comparator: E,
    ) -> Result<Self, Error> {
        if data.len() < (Self::MAC_LEN + 4 + 4 + 4) {
            return Err(km_err!(InvalidKeyBlob, "blob not long enough (len = {})", data.len()));
        }

        // Check the HMAC in the last 8 bytes before doing anything else.
        let mac = &data[data.len() - Self::MAC_LEN..];
        let computed_mac = Self::compute_hmac(hmac, &data[..data.len() - Self::MAC_LEN], hidden)?;
        if comparator.ne(mac, &computed_mac) {
            return Err(km_err!(InvalidKeyBlob, "invalid key blob"));
        }

        let version = consume_u8(&mut data)?;
        if version != KEY_BLOB_VERSION {
            return Err(km_err!(InvalidKeyBlob, "unexpected blob version {}", version));
        }
        let key_material = consume_vec(&mut data)?;
        let hw_enforced = crate::tag::legacy::deserialize(&mut data)?;
        let sw_enforced = crate::tag::legacy::deserialize(&mut data)?;

        // Should just be the (already-checked) MAC left.
        let rest = &data[Self::MAC_LEN..];
        if !rest.is_empty() {
            return Err(km_err!(InvalidKeyBlob, "extra data (len {})", rest.len()));
        }
        Ok(KeyBlob { key_material, hw_enforced, sw_enforced })
    }

    /// Compute the authentication HMAC for a KeyBlob. This is built as:
    ///   HMAC-SHA256(HK, data || serialize(hidden))
    /// with HK = b"IntegrityAssuredBlob0\0".
    pub fn compute_hmac<H: crypto::Hmac>(
        hmac: &H,
        data: &[u8],
        hidden: &[KeyParam],
    ) -> Result<Vec<u8>, crate::Error> {
        let hidden_data = crate::tag::legacy::serialize(hidden)?;
        let mut op = hmac.begin(
            crypto::hmac::Key(try_to_vec(HMAC_KEY)?).into(),
            kmr_wire::keymint::Digest::Sha256,
        )?;
        op.update(data)?;
        op.update(&hidden_data)?;
        let mut tag = op.finish()?;
        tag.truncate(Self::MAC_LEN);
        Ok(tag)
    }
}

/// Build the parameters that are used as the hidden input to HMAC calculations:
/// - `ApplicationId(data)` if present
/// - `ApplicationData(data)` if present
/// - (repeated) `RootOfTrust(rot)` where `rot` is a hardcoded root of trust (expected to
///   be the CBOR serialization of a `RootOfTrustInfo` instance).
pub fn hidden(params: &[KeyParam], rots: &[&[u8]]) -> Result<Vec<KeyParam>, Error> {
    let mut results = Vec::new();
    if let Ok(Some(app_id)) = get_opt_tag_value!(params, ApplicationId) {
        results.try_push(KeyParam::ApplicationId(try_to_vec(app_id)?))?;
    }
    if let Ok(Some(app_data)) = get_opt_tag_value!(params, ApplicationData) {
        results.try_push(KeyParam::ApplicationData(try_to_vec(app_data)?))?;
    }
    for rot in rots {
        results.try_push(KeyParam::RootOfTrust(try_to_vec(rot)?))?;
    }
    Ok(results)
}
