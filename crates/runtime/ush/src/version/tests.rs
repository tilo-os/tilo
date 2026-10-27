use super::*;

use fs_protocol::files::{
    ContentHash, FileActor, FileHistoryPageV1, FileHistoryReadScopeV1, FileVersionAttributionV1,
};
use fs_protocol::{FileAttributionV1, FileRecordId};

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

fn actor() -> FileActor {
    FileActor {
        person: collab_types::Ed25519PublicKey([1; 32]),
        device: collab_types::Ed25519PublicKey([2; 32]),
    }
}

fn version(id: u128, parents: Vec<u128>, change: FileProvenance) -> FileVersionV1 {
    FileVersionV1 {
        id: FileVersionId(uuid::Uuid::from_u128(id)),
        record_id: FileRecordId(uuid::Uuid::from_u128(99)),
        parents: parents
            .into_iter()
            .map(|parent| FileVersionId(uuid::Uuid::from_u128(parent)))
            .collect(),
        actor: actor(),
        timestamp_ms: 1_000 * u64::try_from(id).unwrap_or(1),
        content_hash: Some(ContentHash([u8::try_from(id % 251).unwrap_or(1); 32])),
        metadata_hash: ContentHash([7; 32]),
        provenance: change,
    }
}

fn scope(frontier: [u8; 32]) -> FileHistoryReadScopeV1 {
    FileHistoryReadScopeV1 {
        workspace_id: uuid::Uuid::from_u128(1),
        mount_lifetime: uuid::Uuid::from_u128(2),
        record_id: FileRecordId(uuid::Uuid::from_u128(99)),
        frontier,
    }
}

/// Write what the filesystem answers for one read query onto a real file,
/// exactly as the mount serves it: that is the production transport path, so
/// these tests exercise the same parsing `version log` does.
fn serve(file: &File, query: FileHistoryReadQueryV1, value: FileHistoryReadValueV1) {
    let bytes = serde_json::to_vec(&FileHistoryReadResponseV1 {
        schema_version: 1,
        result: Ok(value),
    })
    .unwrap();
    host_fs::attributes::set_open(file, query.xattr_name(), &bytes).unwrap();
}

fn page(
    frontier: [u8; 32],
    versions: Vec<FileVersionV1>,
    next: Option<FileVersionId>,
) -> FileHistoryPageV1 {
    FileHistoryPageV1 {
        scope: scope(frontier),
        total_versions: u64::try_from(versions.len()).unwrap_or(0),
        versions,
        heads: vec![],
        current_revision: None,
        next_after: next,
        attributions: vec![],
    }
}

#[test]
fn every_verb_is_read_off_its_own_words() {
    let parsed = |line: &[&str]| parse(&words(line)).unwrap().unwrap().command;
    assert_eq!(
        parsed(&["status"]),
        Command::Status { paths: vec![] },
        "no paths means the current directory"
    );
    assert_eq!(
        parsed(&["status", "a.txt", "b.txt"]),
        Command::Status {
            paths: vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]
        }
    );
    assert_eq!(
        parsed(&["log", "notes.txt"]),
        Command::Log {
            path: PathBuf::from("notes.txt")
        }
    );
    assert_eq!(
        parsed(&["show", "notes.txt", "HEAD"]),
        Command::Show {
            path: PathBuf::from("notes.txt"),
            revision: Revision::Head,
        }
    );
    assert_eq!(
        parsed(&["diff", "notes.txt", "abc123"]),
        Command::Diff {
            path: PathBuf::from("notes.txt"),
            from: Revision::Prefix("abc123".into()),
            to: Revision::Head,
        },
        "one revision compares against what the file is now"
    );
    assert_eq!(
        parsed(&["diff", "notes.txt", "abc", "def"]),
        Command::Diff {
            path: PathBuf::from("notes.txt"),
            from: Revision::Prefix("abc".into()),
            to: Revision::Prefix("def".into()),
        }
    );
    let full = "11111111-2222-4333-8444-555555555555";
    assert_eq!(
        parsed(&["restore", "notes.txt", full]),
        Command::Restore {
            path: PathBuf::from("notes.txt"),
            revision: Revision::Id(FileVersionId(uuid::Uuid::parse_str(full).unwrap())),
        },
        "a full id is exact, not a prefix"
    );
}

#[test]
fn a_line_this_command_cannot_read_is_refused_rather_than_guessed() {
    // A path that looks like a flag is refused, so a typo never becomes a
    // filename the caller did not mean.
    for line in [
        words(&["log"]),
        words(&["log", "a", "b"]),
        words(&["show", "a"]),
        words(&["nonsense", "a"]),
        words(&["log", "--force", "a"]),
        words(&["no-such-verb"]),
    ] {
        assert_eq!(parse(&line), Err(ErrorCode::Usage), "{line:?}");
    }
    assert_eq!(parse(&[]).unwrap(), None, "no verb prints the usage");
    // `--json` is a flag this command has, wherever it is written.
    let invocation = parse(&words(&["log", "--json", "a"])).unwrap().unwrap();
    assert!(invocation.json);
    assert_eq!(
        invocation.command,
        Command::Log {
            path: PathBuf::from("a")
        }
    );
}

#[test]
fn a_revision_name_finds_exactly_one_revision_or_says_why_not() {
    let first = uuid::Uuid::from_u128(0xabc000_0000_0000_0000_0000_0000_0001);
    let second = uuid::Uuid::from_u128(0xabc000_0000_0000_0000_0000_0000_0002);
    let unique = uuid::Uuid::from_u128(0xdef000_0000_0000_0000_0000_0000_0003);
    let versions = vec![
        FileVersionV1 {
            id: FileVersionId(first),
            ..version(1, vec![], FileProvenance::Create)
        },
        FileVersionV1 {
            id: FileVersionId(second),
            ..version(2, vec![], FileProvenance::Edit)
        },
        FileVersionV1 {
            id: FileVersionId(unique),
            ..version(3, vec![], FileProvenance::Edit)
        },
    ];

    assert_eq!(
        resolve(&Revision::Head, &versions),
        Ok(FileVersionId(unique)),
        "HEAD is the newest revision the file has"
    );
    assert_eq!(
        resolve(&Revision::Prefix("def".into()), &versions),
        Ok(FileVersionId(unique))
    );
    assert_eq!(
        resolve(&Revision::Id(FileVersionId(first)), &versions),
        Ok(FileVersionId(first))
    );
    assert_eq!(
        resolve(&Revision::Prefix("abc".into()), &versions),
        Err(ErrorCode::Usage),
        "two revisions answer to `abc`, so guessing between them is refused"
    );
    assert_eq!(
        resolve(&Revision::Prefix("999".into()), &versions),
        Err(ErrorCode::NoSuchRevision)
    );
    assert_eq!(
        resolve(
            &Revision::Id(FileVersionId(uuid::Uuid::from_u128(4))),
            &versions
        ),
        Err(ErrorCode::NoSuchRevision),
        "an id this file does not have is not a revision of it"
    );
    assert_eq!(
        resolve(&Revision::Head, &[]),
        Err(ErrorCode::NoSuchRevision),
        "a file with no revisions has no head"
    );
}

#[test]
fn a_diff_names_the_lines_that_changed_and_counts_them() {
    let diff = diff_text("one\ntwo\nthree\n", "one\nTWO\nthree\nfour\n");
    let shown: Vec<(char, &str)> = diff
        .lines
        .iter()
        .map(|line| (line.op, line.text.as_str()))
        .collect();
    assert_eq!(
        shown,
        vec![
            (' ', "one"),
            ('-', "two"),
            ('+', "TWO"),
            (' ', "three"),
            ('+', "four"),
        ]
    );
    assert_eq!((diff.added, diff.removed), (2, 1));
    assert_eq!(diff.lines[2].new_line, Some(2), "a line keeps its number");
    assert_eq!(diff.lines[1].new_line, None, "a removed line has none");
}

#[test]
fn a_trailing_newline_is_a_terminator_not_an_empty_line() {
    assert_eq!(diff_text("a\n", "a\n").lines.len(), 1);
    assert!(diff_text("a\n", "a\n").added == 0);
    assert!(diff_text("", "").lines.is_empty());
    assert_eq!(
        diff_text("", "a\n").lines,
        vec![DiffLine {
            op: '+',
            text: "a".into(),
            old_line: None,
            new_line: Some(1),
        }]
    );
    // A repeated line pairs with its own copy, so inserting one copy does not
    // read as every copy moving.
    let repeated = diff_text("x\nx\n", "x\nx\nx\n");
    assert_eq!((repeated.added, repeated.removed), (1, 0));
}

#[test]
fn a_revision_line_says_what_changed_and_who_did_it() {
    let from = FileVersionId(uuid::Uuid::from_u128(5));
    let restore = version(6, vec![5], FileProvenance::Restore { from });
    let attribution = FileVersionAttributionV1 {
        version_id: restore.id,
        attribution: FileAttributionV1 {
            actor: "agent:scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        },
    };
    let line = RevisionLine::of(&restore, std::slice::from_ref(&attribution));
    assert_eq!(line.change, "restored");
    assert_eq!(line.agent.as_deref(), Some("Scribe"));
    assert_eq!(
        line.restored_from.as_deref(),
        Some("00000000000000000000000000000005")
    );
    assert_eq!(line.parents, vec!["00000000000000000000000000000005"]);

    // Without an attribution the line names nobody rather than guessing a
    // person from the signing device.
    let plain = RevisionLine::of(&restore, &[]);
    assert_eq!(plain.agent, None);
}

#[test]
fn binary_content_is_shown_but_never_diffed() {
    assert!(is_text(b"plain text\n"));
    assert!(!is_text(b"has a\0nul"));
    assert!(!is_text(&[0xff, 0xfe, 0x00]));
}

#[test]
fn a_files_history_is_read_through_the_attributes_the_mount_serves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let file = MountedFile::open(&path).unwrap();

    let first = version(1, vec![], FileProvenance::Create);
    let second = version(2, vec![1], FileProvenance::Edit);
    let frontier = [0xab; 32];

    // The first page holds one revision and says there is another: a caller
    // that stopped here would silently show a truncated log.
    serve(
        &file.file,
        FileHistoryReadQueryV1::Page { cursor: None },
        FileHistoryReadValueV1::Page(page(frontier, vec![first.clone()], Some(first.id))),
    );
    // `log()` re-reads the frontier before asking for page two, which is the
    // same first page; serve the second page against that frontier for the
    // cursor it will use.
    serve(
        &file.file,
        FileHistoryReadQueryV1::Page {
            cursor: Some((frontier, first.id)),
        },
        FileHistoryReadValueV1::Page(page(frontier, vec![second.clone()], None)),
    );

    // The transport asks for the second page with the frontier the first page
    // carried, so replace the first answer with one that also knows the
    // cursor it will be asked about.
    let history = file.log();
    let history = history.unwrap_or_else(|error| panic!("log: {error:?}"));
    assert_eq!(
        history.versions,
        vec![first.clone(), second.clone()],
        "both pages are read, in order"
    );
    assert_eq!(history.lines().len(), 2);
}

#[test]
fn a_file_without_the_projection_has_no_history_rather_than_an_empty_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.txt");
    std::fs::write(&path, "one\n").unwrap();
    let file = MountedFile::open(&path).unwrap();

    // Nothing answers the query: that is `no-history`, not "no revisions",
    // because the two mean different things to the caller.
    assert_eq!(file.log(), Err(ErrorCode::NoHistory));
    assert_eq!(file.frontier(), Err(ErrorCode::NoHistory));
    assert_eq!(
        file.content(FileVersionId::new()),
        Err(ErrorCode::NoHistory)
    );
    assert_eq!(
        file.restore(FileVersionId::new()),
        Err(ErrorCode::NoHistory)
    );
}

#[test]
fn a_refusal_from_the_mount_is_named_rather_than_printed_raw() {
    assert_eq!(read_error(FileHistoryReadErrorV1::Stale), ErrorCode::Stale);
    assert_eq!(
        read_error(FileHistoryReadErrorV1::MissingVersion),
        ErrorCode::NoSuchRevision
    );
    assert_eq!(
        read_error(FileHistoryReadErrorV1::NoContent),
        ErrorCode::NoContent
    );
    assert_eq!(
        read_error(FileHistoryReadErrorV1::NotVisible),
        ErrorCode::Refused
    );
    assert_eq!(
        outcome_refusal(CheckedHistoryOutcomeV1::ReviewRequired),
        ErrorCode::ReviewRequired
    );
    assert_eq!(
        outcome_refusal(CheckedHistoryOutcomeV1::RecoveryPending {
            intent_id: uuid::Uuid::from_u128(1)
        }),
        ErrorCode::RecoveryPending
    );
}

#[test]
fn a_revision_whose_content_is_missing_is_read_as_having_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "one\n").unwrap();
    let file = MountedFile::open(&path).unwrap();
    let frontier = [0xcd; 32];
    // The scope is served, but the preview for this revision is not.
    serve(
        &file.file,
        FileHistoryReadQueryV1::Page { cursor: None },
        FileHistoryReadValueV1::Page(page(
            frontier,
            vec![version(1, vec![], FileProvenance::Create)],
            None,
        )),
    );
    assert_eq!(file.frontier(), Ok(frontier));
    assert_eq!(
        file.content(FileVersionId::new()),
        Err(ErrorCode::NoContent)
    );
}
