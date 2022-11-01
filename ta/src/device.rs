//! Traits representing access to device-specific information and functionality.

use alloc::vec::Vec;
use kmr_common::{
    crypto, crypto::aes, crypto::KeyMaterial, crypto::RawKeyMaterial, keyblob, km_err, Error,
};
use kmr_wire::keymint;

/// Combined collection of trait implementations that must be provided.
pub struct Implementation<'a> {
    /// Retrieval of root key material.
    pub keys: &'a dyn RetrieveKeyMaterial,

    /// Retrieval of attestation certificate signing information.
    pub sign_info: &'a dyn RetrieveCertSigningInfo,

    /// Retrieval of attestation ID information.
    pub attest_ids: Option<&'a mut dyn RetrieveAttestationIds>,

    /// Secure deletion secret manager.  If not available, rollback-resistant
    /// keys will not be supported.
    pub sdd_mgr: Option<&'a mut dyn keyblob::SecureDeletionSecretManager>,

    /// Retrieval of bootloader status.
    pub bootloader: &'a dyn BootloaderStatus,

    /// Storage key wrapping. If not available `convertStorageKeyToEphemeral()` will not be
    /// supported
    pub sk_wrapper: Option<&'a dyn StorageKeyWrapper>,

    /// Trusted user presence indicator.
    pub tup: &'a dyn TrustedUserPresence,
}

/// Retrieval of key material.  The caller is expected to drop the key material as soon as it is
/// done with it.
pub trait RetrieveKeyMaterial {
    /// Retrieve the root key used for derivation of a per-keyblob key encryption key (KEK).
    fn root_kek(&self) -> RawKeyMaterial;

    /// Retrieve the key agreement key used for shared secret negotiation.
    fn kak(&self) -> aes::Key;

    /// Retrieve the hardware backed secret used for UNIQUE_ID generation.
    fn unique_id_hbk(&self, ckdf: Option<&dyn crypto::Ckdf>) -> Result<crypto::hmac::Key, Error> {
        if let Some(ckdf) = ckdf {
            let unique_id_label = b"UniqueID HBK 32B";
            ckdf.ckdf(&self.kak().into(), unique_id_label, &[], 32).map(crypto::hmac::Key::new)
        } else {
            Err(km_err!(Unimplemented, "default impl requires ckdf implementation"))
        }
    }
}

/// Identification of which attestation signing key is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningKey {
    /// Use a batch key that is shared across multiple devices (to prevent the keys being used as
    /// device identifiers).
    Batch,
    /// Use a device-unique key for signing. Only supported for StrongBox.
    DeviceUnique,
}

/// Retrieval of attestation certificate signing information.  The caller is expected to drop key
/// material after use, but may cache public key material.
pub trait RetrieveCertSigningInfo {
    /// Return the signing key material for the specified `key_type`.
    fn signing_key(&self, key_type: SigningKey) -> Result<KeyMaterial, Error>;

    /// Return the certificate chain associated with the specified signing key, where:
    /// - `chain[0]` holds the public key that corresponds to `signing_key`, and which is signed
    ///   by...
    /// - the keypair described by the second entry `chain[1]`, which in turn is signed by...
    /// - ...
    /// - the final certificate in the chain should be a self-signed cert holding a Google root.
    fn cert_chain(&self, key_type: SigningKey) -> Result<Vec<keymint::Certificate>, Error>;
}

/// Retrieval of attestation ID information.  This information will not change (so the caller can
/// cache this information after first invocation).
pub trait RetrieveAttestationIds {
    /// Return the attestation IDs associated with the device, if available.
    fn get(&self) -> Result<crate::AttestationIdInfo, Error>;

    /// Destroy all attestation IDs associated with the device.
    fn destroy_all(&mut self) -> Result<(), Error>;
}

/// Bootloader status.
pub trait BootloaderStatus {
    /// Indication of whether bootloader processing is complete
    fn done(&self) -> bool {
        // By default assume that the bootloader is done before KeyMint starts.
        true
    }
}

/// Marker implementation for implementations that do not support `BOOTLOADER_ONLY` keys, which
/// always indicates that bootloader processing is complete.
struct BootloaderDone;
impl BootloaderStatus for BootloaderDone {}

/// Trusted user presence indicator.
pub trait TrustedUserPresence {
    /// Indication of whether user presence is detected, via a mechanism in the current secure
    /// environment.
    fn available(&self) -> bool {
        // By default assume that trusted user presence is not supported.
        false
    }
}

/// Marker implementation to indicate that trusted user presence is not supported.
pub struct TrustedPresenceUnsupported;
impl TrustedUserPresence for TrustedPresenceUnsupported {}

/// Storage key wrapping.
pub trait StorageKeyWrapper {
    /// Wrap the provided key material using an ephemeral storage key.
    fn ephemeral_wrap(&self, key_material: &KeyMaterial) -> Result<Vec<u8>, Error>;
}
