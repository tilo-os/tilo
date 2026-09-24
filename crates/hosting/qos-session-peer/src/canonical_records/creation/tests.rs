use super::*;

fn row() -> CreationRowV1 {
    let dir = tempfile::tempdir().unwrap();
    let owner =
        crate::SessionOwner::initialize_workspace(&dir.path().join("owner"), "Ledger").unwrap();
    let certificate = owner.peer().authority().clone();
    let key = CreationKeyV1 {
        room: *uuid::Uuid::parse_str(owner.peer().room())
            .unwrap()
            .as_bytes(),
        caller_root: certificate.user_public_key,
        caller_device: certificate.device_public_key,
        invocation: [3; 16],
    };
    let intent = CreationIntentV1 {
        version: 1,
        key: key.clone(),
        original_caller_certificate_digest: certificate_digest(&certificate).unwrap(),
        app: "dev.qos.chat".into(),
        registry_revision: [4; 32],
        source: CreationSourceV1::PublicAction {
            original_action_digest: [5; 32],
            owner_scope: RetainedInvocationScope::Certified {
                root: key.caller_root,
                device: key.caller_device,
            },
            target: PublicTargetV1::Primary,
        },
    };
    let result = CreationResultV1::from_body(CreationResultBodyV1 {
        version: 1,
        key: key.clone(),
        intent_digest: intent.digest().unwrap(),
        route: ResolvedCreationRouteV1 {
            room: key.room,
            app: intent.app.clone(),
            record: String::new(),
            kind: "chat".into(),
        },
        terminal: CreationTerminalV1::ExistingConflict(ConflictV1 {
            observed_canonical_revision: [6; 32],
            observed_checkpoint_digest: [7; 32],
            known_winner_result_id: None,
        }),
        owner_generation: 9,
    })
    .unwrap();
    CreationRowV1 {
        intent,
        original_certificate: certificate,
        original_request: b"original exact action".to_vec(),
        result,
    }
}
#[test]
fn creation_ledger_retains_exact_rows_and_refuses_substitution_without_mutation() {
    let row = row();
    let mut ledger = CreationLedgerV1::default();
    ledger.admit(row.clone()).unwrap();
    let bytes = ledger.encode().unwrap();
    ledger.admit(row.clone()).unwrap();
    assert_eq!(ledger.encode().unwrap(), bytes);
    let mut substitute = row.clone();
    substitute.original_request.push(0);
    assert!(ledger.admit(substitute).is_err());
    assert_eq!(ledger.encode().unwrap(), bytes);
    assert_eq!(CreationLedgerV1::decode(&bytes).unwrap(), ledger);
    assert_eq!(ledger.lookup(&row.intent.key), Some(&row));
}
#[test]
fn creation_ledger_rejects_hash_scope_and_source_bound_tampering() {
    for change in [0, 1, 2, 3] {
        let mut row = row();
        match change {
            0 => row.result.business_binding[0] ^= 1,
            1 => row.intent.key.caller_device[0] ^= 1,
            2 => row
                .original_request
                .resize(MAX_CREATION_SOURCE_BYTES + 1, 0),
            _ => row.result.body.route.app = "dev.qos.notes".into(),
        }
        let mut ledger = CreationLedgerV1::default();
        assert!(ledger.admit(row).is_err());
        assert!(ledger.is_empty());
    }
}
#[test]
fn creation_ledger_empty_literal_is_strict_and_bounded() {
    let empty = CreationLedgerV1::default();
    assert_eq!(empty.encode().unwrap(), [1, 0]);
    assert_eq!(CreationLedgerV1::decode(&[1, 0]).unwrap(), empty);
    for bytes in [
        &[1][..],
        &[2, 0],
        &[1, 0, 0],
        &[0x81, 0, 0],
        &[1, 0xff, 0xff, 0xff, 0xff, 0x7f],
    ] {
        assert!(CreationLedgerV1::decode(bytes).is_err());
    }
    assert!(CreationLedgerV1::decode(&vec![0; MAX_CREATION_LEDGER_BYTES + 1]).is_err());
}

#[test]
fn creation_ledger_byte_pressure_never_discards_acknowledged_rows() {
    let mut base = row();
    base.original_request.resize(MAX_CREATION_SOURCE_BYTES, 42);
    let mut ledger = CreationLedgerV1::default();
    for id in 1u128..65 {
        let mut next = base.clone();
        next.intent.key.invocation = id.to_be_bytes();
        next.result.body.key = next.intent.key.clone();
        next.result.body.intent_digest = next.intent.digest().unwrap();
        next.result = CreationResultV1::from_body(next.result.body).unwrap();
        let before = ledger.encoded_len().unwrap();
        match ledger.admit(next) {
            Ok(()) => assert!(ledger.encoded_len().unwrap() > before),
            Err(_) => {
                assert_eq!(ledger.len(), (id - 1) as usize);
                assert_eq!(ledger.encoded_len().unwrap(), before);
                assert!(before > 3 * 1024 * 1024);
                let bytes = ledger.encode().unwrap();
                assert_eq!(CreationLedgerV1::decode(&bytes).unwrap(), ledger);
                return;
            }
        }
    }
    panic!("actual canonical byte bound must refuse before the 65th maximum-source row");
}
