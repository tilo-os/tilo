use super::*;

fn receipt() -> ScratchReceipt {
    ScratchReceipt {
        parent: PathBuf::from("/var/lib/tilo/capture"),
        parent_device: 1,
        parent_inode: 2,
        name: format!("inventory-{}", uuid::Uuid::nil()),
        device: 1,
        inode: 3,
        caller: 1000,
    }
}

#[test]
fn receipt_wire_round_trip_preserves_exact_custody_facts() {
    let expected = receipt();
    let bytes = serde_json::to_vec(&expected).unwrap();
    let actual: ScratchReceipt = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn receipt_wire_rejects_noncanonical_names_and_unbounded_parents() {
    let original = serde_json::to_value(receipt()).unwrap();
    for name in ["inventory-not-a-uuid", "inventory-../escape", "other"] {
        let mut wire = original.clone();
        wire["name"] = name.into();
        assert!(serde_json::from_value::<ScratchReceipt>(wire).is_err());
    }
    for parent in ["relative", "/var/lib/../elsewhere"] {
        let mut wire = original.clone();
        wire["parent"] = parent.into();
        assert!(serde_json::from_value::<ScratchReceipt>(wire).is_err());
    }
    let mut wire = original;
    wire["unexpected"] = true.into();
    assert!(serde_json::from_value::<ScratchReceipt>(wire).is_err());
}
