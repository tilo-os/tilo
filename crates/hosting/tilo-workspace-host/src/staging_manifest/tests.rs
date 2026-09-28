use super::*;
fn metadata() -> StageManifest {
    StageManifest {
        identity: StageIdentity {
            workspace: 0x0807060504030201,
            device: [9; 32],
            delivery: [10; 16],
            digest: [11; 32],
            total: 64 * 1024 * 1024,
        },
        prefix: 7,
        expires_at_ns: 0x1817161514131211,
        prefix_digest: [12; 32],
    }
}
#[test]
fn staging_manifest_has_exact_148_byte_layout_without_padding() {
    let value = metadata();
    let mut literal = [0; 148];
    literal[..4].copy_from_slice(b"QUP1");
    literal[4..12].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    literal[12..44].fill(9);
    literal[44..60].fill(10);
    literal[60..92].fill(11);
    literal[92..100].copy_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0]);
    literal[100..108].copy_from_slice(&[7, 0, 0, 0, 0, 0, 0, 0]);
    literal[108..116].copy_from_slice(&[17, 18, 19, 20, 21, 22, 23, 24]);
    literal[116..148].fill(12);
    assert_eq!(value.encode().unwrap(), literal);
    assert_eq!(StageManifest::decode(&literal).unwrap(), value);
    for n in 0..148 {
        assert!(StageManifest::decode(&literal[..n]).is_err());
    }
    assert!(StageManifest::decode(&[literal.as_slice(), &[0]].concat()).is_err());
    literal[3] = b'2';
    assert!(StageManifest::decode(&literal).is_err());
}
#[test]
fn staging_manifest_rejects_invalid_scope_bounds_and_prefix_evidence() {
    let mut value = metadata();
    value.identity.workspace = 0;
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.identity.device = [0; 32];
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.identity.delivery = [0; 16];
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.identity.digest = [0; 32];
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.identity.total = 64 * 1024 * 1024 + 1;
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.identity.total = 0;
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.prefix = value.identity.total + 1;
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.expires_at_ns = 0;
    assert!(value.encode().is_err());
    let mut value = metadata();
    value.prefix = 0;
    assert!(value.encode().is_err());
    value.prefix_digest = *blake3::hash(&[]).as_bytes();
    assert!(value.encode().is_ok());
    value.prefix = value.identity.total;
    value.prefix_digest = [12; 32];
    assert!(value.encode().is_ok());
}
