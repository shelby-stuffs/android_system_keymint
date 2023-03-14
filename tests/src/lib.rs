//! Test methods to confirm basic functionality of trait implementations.

use core::convert::TryInto;
use kmr_common::crypto::{
    aes, des, hmac, Aes, AesCmac, Ckdf, ConstTimeEq, Des, Hkdf, Hmac, MonotonicClock, Rng,
    SymmetricOperation,
};
use kmr_common::{keyblob, keyblob::SlotPurpose};
use kmr_wire::keymint::Digest;
use std::collections::HashMap;

/// Test basic [`Rng`] functionality.
pub fn test_rng<R: Rng>(rng: &mut R) {
    let u1 = rng.next_u64();
    let u2 = rng.next_u64();
    assert_ne!(u1, u2);

    let mut b1 = [0u8; 16];
    let mut b2 = [0u8; 16];
    rng.fill_bytes(&mut b1);
    rng.fill_bytes(&mut b2);
    assert_ne!(b1, b2);

    rng.add_entropy(&b1);
    rng.add_entropy(&[]);
    rng.fill_bytes(&mut b1);
    assert_ne!(b1, b2);
}

/// Test basic [`ConstTimeEq`] functionality. Does not test the key constant-time property though.
pub fn test_eq<E: ConstTimeEq>(comparator: E) {
    let b0 = [];
    let b1 = [0u8, 1u8, 2u8];
    let b2 = [1u8, 1u8, 2u8];
    let b3 = [0u8, 1u8, 3u8];
    let b4 = [0u8, 1u8, 2u8, 3u8];
    let b5 = [42; 4096];
    let mut b6 = [42; 4096];
    b6[4095] = 43;
    assert!(comparator.eq(&b0, &b0));
    assert!(comparator.eq(&b5, &b5));

    assert!(comparator.ne(&b0, &b1));
    assert!(comparator.ne(&b0, &b2));
    assert!(comparator.ne(&b0, &b3));
    assert!(comparator.ne(&b0, &b4));
    assert!(comparator.ne(&b0, &b5));
    assert!(comparator.eq(&b1, &b1));
    assert!(comparator.ne(&b1, &b2));
    assert!(comparator.ne(&b1, &b3));
    assert!(comparator.ne(&b1, &b4));
    assert!(comparator.ne(&b5, &b6));
}

/// Test basic [`MonotonicClock`] functionality.
pub fn test_clock<C: MonotonicClock>(clock: C) {
    let t1 = clock.now();
    let t2 = clock.now();
    assert!(t2.0 >= t1.0);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let t3 = clock.now();
    assert!(t3.0 > (t1.0 + 200));
}

/// Test basic HKDF functionality.
pub fn test_hkdf<H: Hmac>(hmac: H) {
    struct TestCase {
        ikm: &'static str,
        salt: &'static str,
        info: &'static str,
        out_len: usize,
        want: &'static str,
    }

    const HKDF_TESTS: &[TestCase] = &[
        // RFC 5869 section A.1
        TestCase {
            ikm: "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
            salt: "000102030405060708090a0b0c",
            info: "f0f1f2f3f4f5f6f7f8f9",
            out_len: 42,
            want: concat!(
                "3cb25f25faacd57a90434f64d0362f2a",
                "2d2d0a90cf1a5a4c5db02d56ecc4c5bf",
                "34007208d5b887185865",
            ),
        },
        // RFC 5869 section A.2
        TestCase {
            ikm: concat!(
                "000102030405060708090a0b0c0d0e0f",
                "101112131415161718191a1b1c1d1e1f",
                "202122232425262728292a2b2c2d2e2f",
                "303132333435363738393a3b3c3d3e3f",
                "404142434445464748494a4b4c4d4e4f",
            ),
            salt: concat!(
                "606162636465666768696a6b6c6d6e6f",
                "707172737475767778797a7b7c7d7e7f",
                "808182838485868788898a8b8c8d8e8f",
                "909192939495969798999a9b9c9d9e9f",
                "a0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
            ),
            info: concat!(
                "b0b1b2b3b4b5b6b7b8b9babbbcbdbebf",
                "c0c1c2c3c4c5c6c7c8c9cacbcccdcecf",
                "d0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
                "e0e1e2e3e4e5e6e7e8e9eaebecedeeef",
                "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            ),
            out_len: 82,
            want: concat!(
                "b11e398dc80327a1c8e7f78c596a4934",
                "4f012eda2d4efad8a050cc4c19afa97c",
                "59045a99cac7827271cb41c65e590e09",
                "da3275600c2f09b8367793a9aca3db71",
                "cc30c58179ec3e87c14c01d5c1f3434f",
                "1d87",
            ),
        },
        // RFC 5869 section A.3
        TestCase {
            ikm: "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
            salt: "",
            info: "",
            out_len: 42,
            want: concat!(
                "8da4e775a563c18f715f802a063c5a31",
                "b8a11f5c5ee1879ec3454e5f3c738d2d",
                "9d201395faa4b61a96c8",
            ),
        },
    ];

    for (i, test) in HKDF_TESTS.iter().enumerate() {
        let ikm = hex::decode(test.ikm).unwrap();
        let salt = hex::decode(test.salt).unwrap();
        let info = hex::decode(test.info).unwrap();

        let got = hmac.hkdf(&salt, &ikm, &info, test.out_len).unwrap();
        assert_eq!(hex::encode(got), test.want, "incorrect HKDF result for case {}", i);
    }
}

/// Test basic [`Hmac`] functionality.
pub fn test_hmac<H: Hmac>(hmac: H) {
    struct TestCase {
        digest: Digest,
        tag_size: usize,
        key: &'static [u8],
        data: &'static [u8],
        expected_mac: &'static str,
    }

    const HMAC_TESTS : &[TestCase] = &[
        TestCase {
            digest: Digest::Sha256,
            tag_size:     32,
            data:         b"Hello",
            key:          b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f",
            expected_mac: "e0ff02553d9a619661026c7aa1ddf59b7b44eac06a9908ff9e19961d481935d4",
        },
        TestCase {
            digest: Digest::Sha512,
            tag_size:     64,
            data:         b"Hello",
            key:          b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f",
            expected_mac: "481e10d823ba64c15b94537a3de3f253c16642451ac45124dd4dde120bf1e5c15e55487d55ba72b43039f235226e7954cd5854b30abc4b5b53171a4177047c9b",
        },
        // empty data
        TestCase {
            digest: Digest::Sha256,
            tag_size:     32,
            data:         &[],
            key:          b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f",
            expected_mac: "07eff8b326b7798c9ccfcbdbe579489ac785a7995a04618b1a2813c26744777d",
        },

        // Test cases from RFC 4231 Section 4.2
        TestCase {
            digest: Digest::Sha224,
            tag_size: 224/8,
            key: &[0x0b; 20],
            data: b"Hi There",
            expected_mac: concat!(
                "896fb1128abbdf196832107cd49df33f",
                "47b4b1169912ba4f53684b22",
            ),
        },
        TestCase {
            digest: Digest::Sha256,
            tag_size: 256/8,
            key: &[0x0b; 20],
            data: b"Hi There",
            expected_mac: concat!(
                "b0344c61d8db38535ca8afceaf0bf12b",
                "881dc200c9833da726e9376c2e32cff7",
            ),
        },
        TestCase {
            digest: Digest::Sha384,
            tag_size: 384/8,
            key: &[0x0b; 20],
            data: b"Hi There",
            expected_mac: concat!(
                "afd03944d84895626b0825f4ab46907f",
                "15f9dadbe4101ec682aa034c7cebc59c",
                "faea9ea9076ede7f4af152e8b2fa9cb6",
            ),
        },
        TestCase {
            digest: Digest::Sha512,
            tag_size: 512/8,
            key: &[0x0b; 20],
            data: b"Hi There",
            expected_mac: concat!(
                "87aa7cdea5ef619d4ff0b4241a1d6cb0",
                "2379f4e2ce4ec2787ad0b30545e17cde",
                "daa833b7d6b8a702038b274eaea3f4e4",
                "be9d914eeb61f1702e696c203a126854"
            ),
        },
        // Test cases from RFC 4231 Section 4.3
        TestCase {
            digest: Digest::Sha224,
            tag_size: 224/8,
            key: b"Jefe",
            data: b"what do ya want for nothing?",
            expected_mac: concat!(
                "a30e01098bc6dbbf45690f3a7e9e6d0f",
                "8bbea2a39e6148008fd05e44"
            ),
        },
        TestCase {
            digest: Digest::Sha256,
            tag_size: 256/8,
            key: b"Jefe",
            data: b"what do ya want for nothing?",
            expected_mac: concat!(
                "5bdcc146bf60754e6a042426089575c7",
                "5a003f089d2739839dec58b964ec3843"
            ),
        },
        TestCase {
            digest: Digest::Sha384,
            tag_size: 384/8,
            key: b"Jefe",
            data: b"what do ya want for nothing?",
            expected_mac: concat!(
                "af45d2e376484031617f78d2b58a6b1b",
                "9c7ef464f5a01b47e42ec3736322445e",
                "8e2240ca5e69e2c78b3239ecfab21649"
            ),
        },
        TestCase {
            digest: Digest::Sha512,
            tag_size: 512/8,
            key: b"Jefe",
            data: b"what do ya want for nothing?",
            expected_mac: concat!(
                "164b7a7bfcf819e2e395fbe73b56e0a3",
                "87bd64222e831fd610270cd7ea250554",
                "9758bf75c05a994a6d034f65f8f0e6fd",
                "caeab1a34d4a6b4b636e070a38bce737"
            ),
        },
        // Test cases from RFC 4231 Section 4.4
        TestCase {
            digest: Digest::Sha224,
            tag_size: 224/8,
            key: &[0xaa; 20],
            data: &[0xdd; 50],
            expected_mac: concat!(
                "7fb3cb3588c6c1f6ffa9694d7d6ad264",
                "9365b0c1f65d69d1ec8333ea"
            ),
        },
        TestCase {
            digest: Digest::Sha256,
            tag_size: 256/8,
            key: &[0xaa; 20],
            data: &[0xdd; 50],
            expected_mac: concat!(
                "773ea91e36800e46854db8ebd09181a7",
                "2959098b3ef8c122d9635514ced565fe"
            ),
        },
        TestCase {
            digest: Digest::Sha384,
            tag_size: 384/8,
            key: &[0xaa; 20],
            data: &[0xdd; 50],
            expected_mac: concat!(
                "88062608d3e6ad8a0aa2ace014c8a86f",
                "0aa635d947ac9febe83ef4e55966144b",
                "2a5ab39dc13814b94e3ab6e101a34f27"
            ),
        },
        TestCase {
            digest: Digest::Sha512,
            tag_size: 512/8,
            key: &[0xaa; 20],
            data: &[0xdd; 50],
            expected_mac: concat!(
                "fa73b0089d56a284efb0f0756c890be9",
                "b1b5dbdd8ee81a3655f83e33b2279d39",
                "bf3e848279a722c806b485a47e67c807",
                "b946a337bee8942674278859e13292fb"
            ),
        },
    ];

    for (i, test) in HMAC_TESTS.iter().enumerate() {
        let mut op = hmac.begin(hmac::Key(test.key.to_vec()).into(), test.digest).unwrap();
        op.update(test.data).unwrap();
        let mut mac = op.finish().unwrap();
        mac.truncate(test.tag_size);

        assert_eq!(
            hex::encode(&mac),
            test.expected_mac[..(test.tag_size * 2)],
            "incorrect mac in test case {}",
            i
        );
    }
}

/// Test basic [`AesCmac`] functionality.
pub fn test_aes_cmac<M: AesCmac>(cmac: M) {
    // Test vectors from RFC 4493.
    let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").expect("Could not decode key");
    let key = aes::Key::new(key).unwrap();
    let data = hex::decode("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710").expect("Could not decode data");
    let expected = vec![
        (0usize, "bb1d6929e95937287fa37d129b756746"),
        (16usize, "070a16b46b4d4144f79bdd9dd04a287c"),
        (40usize, "dfa66747de9ae63030ca32611497c827"),
        (64usize, "51f0bebf7e3b9d92fc49741779363cfe"),
    ]
    .into_iter()
    .collect::<HashMap<usize, &'static str>>();

    for (len, want) in expected {
        let mut op = cmac.begin(key.clone().into()).unwrap();
        op.update(&data[..len]).unwrap();
        let cmac = op.finish().unwrap();

        assert_eq!(hex::encode(&cmac[..16]), want);
    }
}

/// Test `ckdf()` functionality based on an underlying [`AesCmac`] implementation.
pub fn test_ckdf<T: Ckdf>(kdf: T) {
    // Test data manually generated from Android C++ implementation.
    let key = aes::Key::new(vec![0; 32]).unwrap();
    let label = b"KeymasterSharedMac";
    let v0 = vec![0x00, 0x00, 0x00, 0x00];
    let v1 = vec![0x01, 0x01, 0x01, 0x01];
    let v2 = vec![0x02, 0x02, 0x02, 0x02];
    let v3 = vec![0x03, 0x03, 0x03, 0x03];

    let result = kdf.ckdf(&key.into(), label, &[&v0, &v1, &v2, &v3], 32).unwrap();
    assert_eq!(
        hex::encode(result),
        concat!("ac9af88a02241f53d43056a4676c42ee", "f06825755e419e7bd20f4e57487717aa")
    );
}

/// Test AES-GCM functionality.
pub fn test_aes_gcm<A: Aes>(aes: A) {
    struct TestCase {
        key: &'static str,
        iv: &'static str,
        aad: &'static str,
        msg: &'static str,
        ct: &'static str,
        tag: &'static str,
    }
    // Test vectors from https://github.com/google/wycheproof/blob/master/testvectors/aes_gcm_test.json
    let tests = vec![
        TestCase {
            key: "5b9604fe14eadba931b0ccf34843dab9",
            iv: "028318abc1824029138141a2",
            aad: "",
            msg: "001d0c231287c1182784554ca3a21908",
            ct: "26073cc1d851beff176384dc9896d5ff",
            tag: "0a3ea7a5487cb5f7d70fb6c58d038554",
        },
        TestCase {
            key: "5b9604fe14eadba931b0ccf34843dab9",
            iv: "921d2507fa8007b7bd067d34",
            aad: "00112233445566778899aabbccddeeff",
            msg: "001d0c231287c1182784554ca3a21908",
            ct: "49d8b9783e911913d87094d1f63cc765",
            tag: "1e348ba07cca2cf04c618cb4d43a5b92",
        },
    ];
    for test in tests {
        let key = hex::decode(test.key).unwrap();
        let iv = hex::decode(test.iv).unwrap();
        assert_eq!(iv.len(), 12); // Only 96-bit nonces supported.
        let aad = hex::decode(test.aad).unwrap();
        let msg = hex::decode(test.msg).unwrap();
        let tag = hex::decode(test.tag).unwrap();
        assert_eq!(tag.len(), 16); // Test data includes full 128-bit tag

        let aes_key = aes::Key::new(key.clone()).unwrap();
        let mut op = aes
            .begin_aead(
                aes_key.into(),
                aes::GcmMode::GcmTag16 { nonce: iv.clone().try_into().unwrap() },
                SymmetricOperation::Encrypt,
            )
            .unwrap();
        op.update_aad(&aad).unwrap();
        let mut got_ct = op.update(&msg).unwrap();
        got_ct.extend_from_slice(&op.finish().unwrap());
        assert_eq!(format!("{}{}", test.ct, test.tag), hex::encode(&got_ct));

        let aes_key = aes::Key::new(key.clone()).unwrap();
        let mut op = aes
            .begin_aead(
                aes_key.into(),
                aes::GcmMode::GcmTag16 { nonce: iv.clone().try_into().unwrap() },
                SymmetricOperation::Decrypt,
            )
            .unwrap();
        op.update_aad(&aad).unwrap();
        let mut got_pt = op.update(&got_ct).unwrap();
        got_pt.extend_from_slice(&op.finish().unwrap());
        assert_eq!(test.msg, hex::encode(&got_pt));

        // Truncated tag should still decrypt.
        let aes_key = aes::Key::new(key.clone()).unwrap();
        let mut op = match aes.begin_aead(
            aes_key.into(),
            aes::GcmMode::GcmTag12 { nonce: iv.clone().try_into().unwrap() },
            SymmetricOperation::Decrypt,
        ) {
            Ok(c) => c,
            Err(_) => return,
        };
        op.update_aad(&aad).unwrap();
        let mut got_pt = op.update(&got_ct[..got_ct.len() - 4]).unwrap();
        got_pt.extend_from_slice(&op.finish().unwrap());
        assert_eq!(test.msg, hex::encode(&got_pt));

        // Corrupted ciphertext should not decrypt.
        let aes_key = aes::Key::new(key).unwrap();
        let mut op = match aes.begin_aead(
            aes_key.into(),
            aes::GcmMode::GcmTag12 { nonce: iv.try_into().unwrap() },
            SymmetricOperation::Decrypt,
        ) {
            Ok(c) => c,
            Err(_) => return,
        };
        op.update_aad(&aad).unwrap();
        let mut corrupt_ct = got_ct.clone();
        corrupt_ct[0] ^= 0x01;
        let _corrupt_pt = op.update(&corrupt_ct).unwrap();
        let result = op.finish();
        assert!(result.is_err());
    }
}

pub fn test_des<D: Des>(des: D) {
    struct TestCase {
        key: &'static str,
        msg: &'static str,
        ct: &'static str,
    }
    let tests = vec![
        TestCase {
            key: "800000000000000000000000000000000000000000000000",
            msg: "0000000000000000",
            ct: "95a8d72813daa94d",
        },
        TestCase {
            key: "000000000000000000000000000000002000000000000000",
            msg: "0000000000000000",
            ct: "7ad16ffb79c45926",
        },
    ];
    for test in tests {
        let key = hex::decode(test.key).unwrap();
        let msg = hex::decode(test.msg).unwrap();

        let des_key = des::Key::new(key.clone()).unwrap();
        let mut op = des
            .begin(des_key.clone().into(), des::Mode::EcbNoPadding, SymmetricOperation::Encrypt)
            .unwrap();
        let mut got_ct = op.update(&msg).unwrap();
        got_ct.extend_from_slice(&op.finish().unwrap());
        assert_eq!(test.ct, hex::encode(&got_ct));

        let mut op = des
            .begin(des_key.into(), des::Mode::EcbNoPadding, SymmetricOperation::Decrypt)
            .unwrap();
        let mut got_pt = op.update(&got_ct).unwrap();
        got_pt.extend_from_slice(&op.finish().unwrap());
        assert_eq!(test.msg, hex::encode(&got_pt));
    }
}

pub fn test_sdd_mgr<M: keyblob::SecureDeletionSecretManager, R: Rng>(mut sdd_mgr: M, mut rng: R) {
    let (slot1, sdd1) = sdd_mgr.new_secret(&mut rng, SlotPurpose::KeyGeneration).unwrap();
    assert!(sdd_mgr.get_secret(slot1).unwrap() == sdd1);
    assert!(sdd_mgr.get_secret(slot1).unwrap() == sdd1);

    // A second instance should share factory reset secret but not per-key secret.
    let (slot2, sdd2) = sdd_mgr.new_secret(&mut rng, SlotPurpose::KeyGeneration).unwrap();
    assert!(sdd_mgr.get_secret(slot2).unwrap() == sdd2);
    assert_eq!(sdd1.factory_reset_secret, sdd2.factory_reset_secret);
    assert_ne!(sdd1.secure_deletion_secret, sdd2.secure_deletion_secret);

    assert!(sdd_mgr.delete_secret(slot1).is_ok());
    assert!(sdd_mgr.get_secret(slot1).is_err());
    assert!(sdd_mgr.delete_secret(slot1).is_err());

    assert!(sdd_mgr.delete_secret(slot2).is_ok());
}
