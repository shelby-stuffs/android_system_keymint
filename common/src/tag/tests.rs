use super::*;
use crate::{expect_err, hex_decode, hex_encode};
use alloc::vec;
use kmr_wire::{keymint::KeyParam, KeySizeInBits};

#[test]
fn test_characteristics_invalid() {
    let tests = vec![
        (vec![KeyParam::UsageCountLimit(42), KeyParam::UsageCountLimit(43)], "duplicate value"),
        (vec![KeyParam::Nonce(vec![1, 2])], "not a valid key characteristic"),
    ];
    for (characteristics, msg) in tests {
        let result = crate::tag::characteristics_valid(&characteristics);
        expect_err!(result, msg);
    }
}

#[test]
fn test_legacy_serialization() {
    let tests = vec![(
        concat!(
            "00000000", // blob data size
            "03000000", // param count
            "15000000", // param size
            "02000010", // Tag::ALGORITHM = 268435458 = 0x10000002,
            "20000000", // Algorithm::AES
            "03000030", // Tag::KEY_SIZE = 805306371 = 0x30000003
            "00010000", // size = 0x00000100
            "fb010070", // Tag::TRUSTED_USER_PRESENCE_REQUIRED = 0x700001fb
            "01",       // True
        ),
        vec![
            KeyParam::Algorithm(Algorithm::Aes),
            KeyParam::KeySize(KeySizeInBits(256)),
            KeyParam::TrustedUserPresenceRequired,
        ],
    )];

    for (hex_data, want_params) in tests {
        let want_data = hex_decode(hex_data).unwrap();

        let got_data = legacy::serialize(&want_params).unwrap();
        assert_eq!(hex_encode(&got_data), hex_data);

        let mut data = &want_data[..];
        let got_params = legacy::deserialize(&mut data).unwrap();
        assert!(data.is_empty(), "data left: {}", hex_encode(data));
        assert_eq!(got_params, want_params);
    }
}

#[test]
fn test_check_begin_params_fail() {
    let chars = vec![
        KeyParam::NoAuthRequired,
        KeyParam::Algorithm(Algorithm::Hmac),
        KeyParam::KeySize(KeySizeInBits(128)),
        KeyParam::Digest(Digest::Sha256),
        KeyParam::Purpose(KeyPurpose::Sign),
        KeyParam::Purpose(KeyPurpose::Verify),
        KeyParam::MinMacLength(160),
    ];

    let tests = vec![
        (
            KeyPurpose::Encrypt,
            vec![KeyParam::Digest(Digest::Sha256), KeyParam::MacLength(160)],
            "invalid purpose Encrypt",
        ),
        (KeyPurpose::Sign, vec![KeyParam::Digest(Digest::Sha256)], "MissingMacLength"),
        (
            KeyPurpose::Sign,
            vec![KeyParam::Digest(Digest::Sha512), KeyParam::MacLength(160)],
            "not in key characteristics",
        ),
    ];
    for (purpose, params, msg) in tests {
        expect_err!(check_begin_params(&chars, purpose, &params), msg);
    }
}

#[test]
fn test_copyable_tags() {
    for tag in UNPOLICED_COPYABLE_TAGS {
        let info = info(*tag).unwrap();
        assert!(info.user_can_specify.0, "tag {:?} not listed as user-specifiable", tag);
        assert!(
            info.characteristic == info::Characteristic::KeyMintEnforced
                || info.characteristic == info::Characteristic::KeystoreEnforced
                || info.characteristic == info::Characteristic::BothEnforced,
            "tag {:?} with unexpected characteristic {:?}",
            tag,
            info.characteristic
        );
    }
}
