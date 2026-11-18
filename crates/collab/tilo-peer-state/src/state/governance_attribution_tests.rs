//! Every governance change says who wrote it and when. A change nobody stamps carries
//! automerge's zero time, and the workspace document's actor is a device's key, or the one a server
//! writes a workspace it keeps with: neither names a member, and the history then reads "Unknown
//! member" and 1970.
use super::*;
use automerge::Change;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
/// The second the workspace is made and named at, so a stamp of 0 is the clock rather than
/// the caller's time.
const MADE_AT: u64 = 1_800_000_000;

fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// Each change's time, and the person its entry names.
fn stamps(changes: &[Vec<u8>]) -> Vec<(i64, Option<String>)> {
    changes
        .iter()
        .map(|bytes| {
            let change = Change::from_bytes(bytes.clone()).unwrap();
            let entry = change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message);
            let person = entry
                .as_ref()
                .and_then(|entry| entry.acting().and_then(|acting| acting.person_id()))
                .map(str::to_owned);
            (change.timestamp(), person)
        })
        .collect()
}

tilo_test_env::scenario! {
    fn governance_changes_name_their_writer_and_their_time;

    phase a_founded_workspace_and_its_rename_carry_both;
    {
        let root = SigningKey::from_bytes(&[1; 32]);
        let made = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(MADE_AT))
            .unwrap();
        let (key, cert) = device(&root, 2);
        let roster = vec![cert.clone()];
        let founder = hex::encode(cert.user_public_key);
        let mut peer = StatePeer::new(
            WORKSPACE,
            cert.clone(),
            roster.clone(),
            RevocationList::new(),
            made,
        )
        .unwrap();
        peer.install_seed(
            workspace_seed(WORKSPACE, &cert, &roster, "Workspace", made).unwrap(),
            &key,
            made,
        )
        .unwrap();
        // The workspace the seed made: its creation and its naming are someone's changes too.
        let seeded = peer.workspace_changes_since(&[]);
        assert!(!seeded.is_empty(), "the seed wrote the workspace");
        for (time, person) in stamps(&seeded) {
            assert_eq!(time, i64::try_from(MADE_AT).unwrap(), "a governance change is stamped when it is made");
            assert_eq!(person.as_deref(), Some(founder.as_str()), "the founder's change names them");
        }
        let heads = peer.workspace_heads().unwrap();
        let renaming = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(MADE_AT + 60))
            .unwrap();
        peer.governance_mut()
            .author_workspace_name("Named", &key, renaming)
            .unwrap();
        let written = peer.workspace_changes_since(&heads);
        assert_eq!(
            written.len(),
            2,
            "the rename, and the operation recording what it moved"
        );
        let renamed_at = i64::try_from(MADE_AT + 60).unwrap();
        for (time, person) in stamps(&written) {
            assert_eq!(time, renamed_at, "the rename's own time");
            assert_eq!(person.as_deref(), Some(founder.as_str()), "the renamer's change names them");
        }
    }
}
