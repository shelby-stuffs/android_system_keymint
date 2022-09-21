//! Functionality related to AES encryption

use super::{nonce, KeySizeInBits, OutputSize, Rng};
use crate::{
    get_tag_value, km_err, tag,
    wire::keymint::{BlockMode, ErrorCode, KeyParam, PaddingMode},
    Error,
};
use alloc::vec::Vec;
use core::convert::TryInto;

/// Size of an AES block in bytes.
pub const BLOCK_SIZE: usize = 16;

/// Size of AES-GCM nonce in bytes.
pub const GCM_NONCE_SIZE: usize = 12; // 96 bits

/// AES variant.
#[derive(Clone)]
pub enum Variant {
    Aes128,
    Aes192,
    Aes256,
}

/// An AES-128, AES-192 or AES-256 key.
#[derive(Clone, PartialEq, Eq)]
pub enum Key {
    Aes128([u8; 16]),
    Aes192([u8; 24]),
    Aes256([u8; 32]),
}

impl Key {
    /// Create a new [`Key`] from raw data, which must be 16, 24 or 32 bytes long.
    pub fn new(data: Vec<u8>) -> Result<Self, Error> {
        match data.len() {
            16 => Ok(Key::Aes128(data.try_into().unwrap())), // safe: len checked
            24 => Ok(Key::Aes192(data.try_into().unwrap())), // safe: len checked
            32 => Ok(Key::Aes256(data.try_into().unwrap())), // safe: len checked
            l => Err(km_err!(UnsupportedKeySize, "AES keys must be 16, 24 or 32 bytes not {}", l)),
        }
    }
    /// Create a new [`Key`] from raw data, which must be 16, 24 or 32 bytes long.
    pub fn new_from(data: &[u8]) -> Result<Self, Error> {
        Key::new(data.to_vec())
    }

    /// Indicate the size of the key in bits.
    pub fn size(&self) -> KeySizeInBits {
        KeySizeInBits(match self {
            Key::Aes128(_) => 128,
            Key::Aes192(_) => 192,
            Key::Aes256(_) => 256,
        })
    }
}

/// Mode of AES plain cipher operation.  Associated value is the nonce.
#[derive(Clone, Copy, Debug)]
pub enum CipherMode {
    EcbNoPadding,
    EcbPkcs7Padding,
    CbcNoPadding { nonce: [u8; BLOCK_SIZE] },
    CbcPkcs7Padding { nonce: [u8; BLOCK_SIZE] },
    Ctr { nonce: [u8; BLOCK_SIZE] },
}

/// Mode of AES-GCM operation.  Associated value is the nonce.
#[derive(Clone, Copy, Debug)]
pub enum GcmMode {
    GcmTag12 { nonce: [u8; GCM_NONCE_SIZE] },
    GcmTag13 { nonce: [u8; GCM_NONCE_SIZE] },
    GcmTag14 { nonce: [u8; GCM_NONCE_SIZE] },
    GcmTag15 { nonce: [u8; GCM_NONCE_SIZE] },
    GcmTag16 { nonce: [u8; GCM_NONCE_SIZE] },
}

/// Mode of AES operation.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Cipher(CipherMode),
    Aead(GcmMode),
}

impl Mode {
    /// Determine the [`Mode`], rejecting invalid parameters. Use `caller_nonce` if provided,
    /// otherwise generate a new nonce using the provided [`Rng`] instance.
    pub fn new(
        params: &[KeyParam],
        caller_nonce: Option<&Vec<u8>>,
        rng: &mut dyn Rng,
    ) -> Result<Self, Error> {
        let mode = tag::get_block_mode(params)?;
        let padding = tag::get_padding_mode(params)?;
        match mode {
            BlockMode::Ecb => {
                if caller_nonce.is_some() {
                    return Err(km_err!(InvalidNonce, "nonce unexpectedly provided for AES-ECB"));
                }
                match padding {
                    PaddingMode::None => Ok(Mode::Cipher(CipherMode::EcbNoPadding)),
                    PaddingMode::Pkcs7 => Ok(Mode::Cipher(CipherMode::EcbPkcs7Padding)),
                    _ => Err(km_err!(
                        IncompatiblePaddingMode,
                        "expected NONE/PKCS7 padding for AES-ECB"
                    )),
                }
            }
            BlockMode::Cbc => {
                let nonce: [u8; BLOCK_SIZE] =
                    nonce(BLOCK_SIZE, caller_nonce, rng)?.try_into().map_err(|_e| {
                        km_err!(InvalidNonce, "want {} byte nonce for AES-CBC", BLOCK_SIZE)
                    })?;
                match padding {
                    PaddingMode::None => Ok(Mode::Cipher(CipherMode::CbcNoPadding { nonce })),
                    PaddingMode::Pkcs7 => Ok(Mode::Cipher(CipherMode::CbcPkcs7Padding { nonce })),
                    _ => Err(km_err!(
                        IncompatiblePaddingMode,
                        "expected NONE/PKCS7 padding for AES-CBC"
                    )),
                }
            }
            BlockMode::Ctr => {
                if padding != PaddingMode::None {
                    return Err(km_err!(
                        IncompatiblePaddingMode,
                        "expected NONE padding for AES-CTR"
                    ));
                }
                let nonce: [u8; BLOCK_SIZE] =
                    nonce(BLOCK_SIZE, caller_nonce, rng)?.try_into().map_err(|_e| {
                        km_err!(InvalidNonce, "want {} byte nonce for AES-CTR", BLOCK_SIZE)
                    })?;
                Ok(Mode::Cipher(CipherMode::Ctr { nonce }))
            }
            BlockMode::Gcm => {
                if padding != PaddingMode::None {
                    return Err(km_err!(
                        IncompatiblePaddingMode,
                        "expected NONE padding for AES-GCM"
                    ));
                }
                let nonce: [u8; GCM_NONCE_SIZE] = nonce(GCM_NONCE_SIZE, caller_nonce, rng)?
                    .try_into()
                    .map_err(|_e| km_err!(InvalidNonce, "want 12 byte nonce for AES-GCM"))?;
                let tag_len = get_tag_value!(params, MacLength, ErrorCode::InvalidMacLength)?;
                if tag_len % 8 != 0 {
                    return Err(km_err!(
                        InvalidMacLength,
                        "tag length {} not a multiple of 8",
                        tag_len
                    ));
                }
                match tag_len / 8 {
                    12 => Ok(Mode::Aead(GcmMode::GcmTag12 { nonce })),
                    13 => Ok(Mode::Aead(GcmMode::GcmTag13 { nonce })),
                    14 => Ok(Mode::Aead(GcmMode::GcmTag14 { nonce })),
                    15 => Ok(Mode::Aead(GcmMode::GcmTag15 { nonce })),
                    16 => Ok(Mode::Aead(GcmMode::GcmTag16 { nonce })),
                    v => Err(km_err!(
                        InvalidMacLength,
                        "want 12-16 byte tag for AES-GCM not {} bytes",
                        v
                    )),
                }
            }
        }
    }

    /// Indicate whether the AES mode is an AEAD.
    pub fn is_aead(&self) -> bool {
        match self {
            Mode::Aead(_) => true,
            Mode::Cipher(_) => false,
        }
    }
}

impl GcmMode {
    /// Return the tag length (in bytes) for an AES-GCM mode.
    pub fn tag_len(&self) -> usize {
        match self {
            GcmMode::GcmTag12 { nonce: _ } => 12,
            GcmMode::GcmTag13 { nonce: _ } => 13,
            GcmMode::GcmTag14 { nonce: _ } => 14,
            GcmMode::GcmTag15 { nonce: _ } => 15,
            GcmMode::GcmTag16 { nonce: _ } => 16,
        }
    }
}

impl OutputSize for Mode {
    fn update_max_output_len(&self, input_len: usize) -> usize {
        match self {
            Mode::Cipher(CipherMode::EcbNoPadding)
            | Mode::Cipher(CipherMode::EcbPkcs7Padding)
            | Mode::Cipher(CipherMode::CbcNoPadding { nonce: _ })
            | Mode::Cipher(CipherMode::CbcPkcs7Padding { nonce: _ }) => {
                // Block modes - worst case: (BLOCK_SIZE-1) bytes of input already accumulated.
                let accumulated_len = BLOCK_SIZE - 1 + input_len;
                let full_block_count = accumulated_len / BLOCK_SIZE;
                full_block_count * BLOCK_SIZE
            }
            Mode::Cipher(CipherMode::Ctr { nonce: _ })
            | Mode::Aead(GcmMode::GcmTag12 { nonce: _ })
            | Mode::Aead(GcmMode::GcmTag13 { nonce: _ })
            | Mode::Aead(GcmMode::GcmTag14 { nonce: _ })
            | Mode::Aead(GcmMode::GcmTag15 { nonce: _ })
            | Mode::Aead(GcmMode::GcmTag16 { nonce: _ }) => {
                // Stream modes: can emit one byte for each input byte.
                input_len
            }
        }
    }

    fn finish_max_output_len(&self) -> usize {
        match self {
            Mode::Cipher(CipherMode::EcbNoPadding)
            | Mode::Cipher(CipherMode::CbcNoPadding { nonce: _ }) => {
                // Unpadded block mode: caller must have provided input in whole blocks.
                0
            }
            Mode::Cipher(CipherMode::EcbPkcs7Padding)
            | Mode::Cipher(CipherMode::CbcPkcs7Padding { nonce: _ }) => {
                // Padded block modes - worst case: emit one block of padding
                BLOCK_SIZE
            }
            Mode::Cipher(CipherMode::Ctr { nonce: _ }) => {
                // Counter mode - unauthenticated stream cipher, so we're done.
                0
            }
            // AES-GCM is a tagged stream mode, finish emits the tag.
            Mode::Aead(GcmMode::GcmTag12 { nonce: _ }) => 12,
            Mode::Aead(GcmMode::GcmTag13 { nonce: _ }) => 13,
            Mode::Aead(GcmMode::GcmTag14 { nonce: _ }) => 14,
            Mode::Aead(GcmMode::GcmTag15 { nonce: _ }) => 15,
            Mode::Aead(GcmMode::GcmTag16 { nonce: _ }) => 16,
        }
    }
}
