use super::*;
use ed25519_dalek::SigningKey;

struct Fixture {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    owner: NativeWorkspaceOwner,
    workspace: WorkspaceId,
    device: tilo_user_identity::DeviceCert,
    now: SystemTime,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let path = directory.path().join("owner.json");
        let owner = crate::owner::open_file(&path).unwrap();
        let key = SigningKey::from_bytes(&[61; 32]);
        let governor =
            crate::owner::testing::certificate(&SigningKey::from_bytes(&[62; 32]), &key, "Owner");
        crate::owner::testing::enroll_interactive_governor(&owner, &key, &governor).unwrap();
        Self {
            _directory: directory,
            path,
            owner,
            workspace: WorkspaceId::parse(crate::owner::testing::WORKSPACE).unwrap(),
            device: governor,
            now: SystemTime::now(),
        }
    }
    fn person(&self) -> CallerAuthority {
        CallerAuthority::Person {
            caller: self.device.clone(),
        }
    }
    fn store(&self) -> OldDirectConversations {
        self.owner
            .with_current(self.workspace, |view| Ok(view.clone()))
            .unwrap()
            .record
            .old_direct_conversations
    }
    fn seed(&self) -> Result<(), String> {
        self.owner.seed_old_direct_conversations(
            self.workspace,
            &hex::encode([73u8; 32]),
            &["before the records", "and one more"],
            &["the other person's answer"],
            self.now,
        )
    }
    fn read(&self, after: u64, limit: usize) -> Result<Option<OldConversation>, String> {
        self.owner.old_direct_conversation(
            self.workspace,
            &person_store_key([73u8; 32]),
            &self.person(),
            OldConversationPage {
                after_sequence: after,
                limit,
            },
            self.now,
        )
    }
}

/// A conversation the old store held reads back page by page, and the device
/// that emptied it holds none of it afterwards — in the file as well as here.
#[test]
fn a_stored_conversation_reads_in_pages_and_the_store_empties_once() {
    let fixture = Fixture::new();
    fixture.seed().unwrap();
    let first = fixture.read(0, 2).unwrap().expect("a stored conversation");
    assert_eq!(first.messages.len(), 2);
    assert!(first.has_more);
    assert_eq!(first.next_sequence, 2);
    assert_eq!(first.messages[0].text, "before the records");
    let second = fixture
        .read(first.next_sequence, 2)
        .unwrap()
        .expect("the rest of it");
    assert_eq!(second.messages.len(), 1);
    assert!(!second.has_more);
    assert_eq!(second.messages[0].text, "the other person's answer");
    assert_eq!(
        second.messages[0].sender,
        person_store_key([73u8; 32]),
        "the store names the other person by their key"
    );

    fixture
        .owner
        .clear_old_direct_conversations(fixture.workspace, &fixture.person(), fixture.now)
        .unwrap();
    assert!(fixture.read(0, 10).unwrap().is_none());
    let reopened = crate::owner::open_file(&fixture.path).unwrap();
    let record = reopened
        .with_current(fixture.workspace, |view| Ok(view.clone()))
        .unwrap()
        .record;
    assert!(
        record.old_direct_conversations.is_empty(),
        "the emptied store is what the file holds"
    );
}

/// A person with no conversation in the store is no conversation, not an
/// error: the import asks about every person the workspace has.
#[test]
fn a_person_the_store_never_held_answers_nothing() {
    let fixture = Fixture::new();
    fixture.seed().unwrap();
    let stranger = fixture
        .owner
        .old_direct_conversation(
            fixture.workspace,
            &person_store_key([74u8; 32]),
            &fixture.person(),
            OldConversationPage::default(),
            fixture.now,
        )
        .unwrap();
    assert!(stranger.is_none());
}

/// A device that is nobody here reads nothing of the conversations kept here.
#[test]
fn only_a_persons_own_device_reads_the_store() {
    let fixture = Fixture::new();
    fixture.seed().unwrap();
    let stranger = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[81; 32]),
        &SigningKey::from_bytes(&[82; 32]),
        "Stranger",
    );
    let refused = fixture.owner.old_direct_conversation(
        fixture.workspace,
        &person_store_key([73u8; 32]),
        &CallerAuthority::Person { caller: stranger },
        OldConversationPage::default(),
        fixture.now,
    );
    assert!(refused.is_err(), "a stranger reads no conversation");
}

/// A file no store wrote imports nothing rather than nonsense.
#[test]
fn a_corrupt_store_is_refused() {
    let fixture = Fixture::new();
    fixture.seed().unwrap();
    let mut broken = fixture.store();
    assert!(broken.validate().is_ok());
    for pair in broken.pairs.values_mut() {
        pair.messages[0].sequence = 7;
    }
    assert!(broken.validate().is_err());
}
