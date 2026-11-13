//! A workspace a relay governs, its files held by the member that takes the
//! workspace's work and by a person's device, each a runtime of its own; and
//! the same files read by a device that joins once the member that wrote them
//! has left, from what the relay kept of them.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay, the runtimes and the person's device live for the whole scenario"
)]

use super::super::super::Standalone;
use super::super::super::tests::{isolated_command, isolated_home, isolated_root};
use crate::RuntimeServices as _;
use ed25519_dalek::SigningKey;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tilo_peer_state::join::MemberRole;
use tilo_relay::workspaces::IssueInvitationRequest;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

const TEST: &str = "standalone::files::replica::tests::\
    a_file_made_on_the_vm_host_reaches_a_persons_device_and_their_edit_reaches_the_vm_host";
/// The invitation the person's device redeems, in its own process.
const PERSON_INVITATION: &str = "TILO_REPLICA_TEST_PERSON_INVITATION";
const KEPT_TEST: &str = "standalone::files::replica::tests::\
    a_file_the_relay_kept_reaches_a_device_that_joins_after_its_writer_left";
const RETURN_TEST: &str = "standalone::files::replica::tests::\
    content_the_relay_let_go_of_comes_back_from_the_writer_that_was_away";
/// The invitation the device that joins last redeems, in its own process.
const JOINER_INVITATION: &str = "TILO_REPLICA_TEST_JOINER_INVITATION";
const WORKSPACE_NAME: &str = "Project";
/// How much sooner than the test's own bound a wait fails, so it fails
/// with what the folders hold rather than as a hang.
const WAIT_LEAD: Duration = Duration::from_secs(20);

/// `future`, run to its end on the runtime the profile's services run on.
fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tilo_rt::app().block_on(Box::pin(future)).unwrap()
}

/// A device certificate of `root` for `key`, as an enrollment issues one.
fn certificate(
    root: &SigningKey,
    key: &SigningKey,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "fixture".into(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(60_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// The relay tilo.sh runs: a certified host of its operator, governing the
/// workspaces it creates.
async fn relay(state: &Path, operator: &SigningKey) -> tilo_relay::Relay {
    let key = SigningKey::from_bytes(&tilo_test_env::node_key());
    // It owns the workspaces it governs, so it takes part in their channels
    // and their records are sealed.
    let cert = certificate(
        operator,
        &key,
        DeviceCapabilities {
            can_own_workspaces: true,
            ..DeviceCapabilities::host()
        },
    );
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    tilo_relay::Relay::start_on_endpoint_with_protocols(
        tilo_relay::RelayConfig {
            state_root: state.join("state"),
            identity_root: state.join("identity"),
            full_replica_status: state.join("replica/status.json"),
            home_peer_bundle: state.join("home-peer.toml"),
            ..Default::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: operator.verifying_key().to_bytes(),
            device_signing_key: key,
            device_cert: cert,
            revocations: RevocationList::new(),
        },
        endpoint,
        Vec::new(),
    )
    .await
    .unwrap()
}

/// An editor's invitation to `workspace`, as text, that the operator's own
/// device `desk` issues; for anyone's device when `person` is `None`.
async fn invitation(
    relay: &tilo_relay::Relay,
    workspace: &str,
    desk: &DeviceCert,
    person: Option<[u8; 32]>,
    request: u8,
) -> String {
    let issued = relay
        .workspaces()
        .unwrap()
        .issue(
            workspace,
            desk,
            IssueInvitationRequest {
                request_id: [request; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: person,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation)
}

/// Wait until `path` holds exactly `bytes`, failing with what it holds.
fn wait_for_file(path: &Path, bytes: &[u8], what: &str, log: &Path) {
    let started = std::time::Instant::now();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    let arrived = loop {
        if std::fs::read(path).is_ok_and(|held| held == bytes) {
            break true;
        }
        if started.elapsed() > bound {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        arrived,
        "{what}: {} holds {:?} bytes\nthe person's device:\n{}\n{}",
        path.display(),
        std::fs::read(path).ok().map(|held| held.len()),
        std::fs::read_to_string(log).unwrap_or_default(),
        std::fs::read_to_string(log.with_extension("err")).unwrap_or_default(),
    );
}

/// Wait until the relay's blobs under `dir` take `bytes` together, failing
/// with how much they take.
/// What the files under `dir` take, every directory below it included.
fn bytes_under(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| match entry.file_type() {
                    Ok(kind) if kind.is_dir() => bytes_under(&entry.path()),
                    _ => entry.metadata().map_or(0, |kept| kept.len()),
                })
                .fold(0, u64::saturating_add)
        })
        .unwrap_or_default()
}

fn wait_for_kept(dir: &Path, bytes: u64, what: &str) {
    let started = std::time::Instant::now();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    loop {
        let kept = bytes_under(dir);
        if kept >= bytes {
            return;
        }
        assert!(
            started.elapsed() <= bound,
            "{what}: the relay keeps {kept} bytes of them under {}",
            dir.display(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Bytes no two calls with different `seed`s share, `length` of them, with no
/// stretch of them repeating: content is kept by its hash, so a file whose
/// chunks repeat takes less room than its length wherever it is kept.
fn content(seed: u32, length: usize) -> Vec<u8> {
    (0..length)
        .map(|at| {
            // SplitMix64 over the seed and the position.
            let at = u64::try_from(at).unwrap();
            let mut mixed = (u64::from(seed) << 40 | at).wrapping_add(0x9e37_79b9_7f4a_7c15);
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (mixed ^ (mixed >> 31)).to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn a_file_made_on_the_vm_host_reaches_a_persons_device_and_their_edit_reaches_the_vm_host() {
    // The person's device: a runtime of its own that joins the workspace and
    // holds its files until the test stops it.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=debug,tilo_runtime=debug,tilo_channels=debug")
        .with_writer(std::io::stderr)
        .try_init();
    if let Some(invitation) = std::env::var_os(PERSON_INVITATION) {
        let profile = isolated_root(TEST).unwrap();
        let device = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
        device.joiner().unwrap()(invitation.to_str().unwrap()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let Some(profile) = isolated_root(TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();

    // The VM host's member: the runtime that takes the workspace's work.
    let member = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, member_device) =
        tilo_identity_host::broker::trusted_current_device(member.workspace()).unwrap();

    // The relay governs the workspace; its operator's desk owns it.
    let operator = SigningKey::from_bytes(&[41; 32]);
    let desk = certificate(
        &operator,
        &SigningKey::from_bytes(&[43; 32]),
        DeviceCapabilities::primary(),
    );
    let relay = run(relay(&scenario.join("relay"), &operator));
    let workspaces = relay.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(&created.workspace, &desk)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();

    // The member joins and holds the workspace's files at a folder.
    let joined = member.joiner().unwrap()(&run(invitation(
        &relay,
        &workspace,
        &desk,
        Some(member_device.user_public_key),
        1,
    )))
    .unwrap();
    assert_eq!(joined, workspace);
    let member_folder = member
        .joined_files_folder(&workspace)
        .expect("the member holds the joined workspace's files");
    assert_eq!(
        member_folder,
        isolated_home(&scenario).join("Tilo").join(WORKSPACE_NAME)
    );

    // The person's device joins in a process of its own.
    let person_root = scenario.join("person");
    std::fs::create_dir_all(&person_root).unwrap();
    let log = person_root.join("device.log");
    let mut person = isolated_command(TEST, &person_root)
        .env(
            PERSON_INVITATION,
            run(invitation(&relay, &workspace, &desk, None, 2)),
        )
        .env(
            tilo_test_env::hang::HANG_BOUND_ENV,
            super::super::super::tests::helper_hang_bound(),
        )
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::fs::File::create(person_root.join("device.err")).unwrap())
        .spawn_locked()
        .unwrap();
    let person_folder: PathBuf = isolated_home(&person_root)
        .join("Tilo")
        .join(WORKSPACE_NAME);

    // A command in the workspace's VM writes into the member's folder: the
    // person's device holds the same bytes, a small file and one large
    // enough to be kept as file-backed content alike.
    let report = content(1, 300_000);
    let build = content(2, 3 * 1024 * 1024);
    std::fs::create_dir_all(member_folder.join("out")).unwrap();
    std::fs::write(member_folder.join("report.txt"), &report).unwrap();
    std::fs::write(member_folder.join("out").join("build.bin"), &build).unwrap();
    wait_for_file(
        &person_folder.join("report.txt"),
        &report,
        "the file the VM made, on the person's device",
        &log,
    );
    wait_for_file(
        &person_folder.join("out").join("build.bin"),
        &build,
        "the large file the VM made, on the person's device",
        &log,
    );

    // The person edits on their device: the VM host's folder holds the edit.
    let edited = content(3, 40_000);
    std::fs::write(person_folder.join("report.txt"), &edited).unwrap();
    wait_for_file(
        &member_folder.join("report.txt"),
        &edited,
        "the person's edit, in the VM host's folder",
        &log,
    );

    let _ = person.kill();
    let _ = person.wait();
    drop(member);
    run(relay.shutdown());
}

/// The files a relay keeps reach a device that joins after the member that
/// wrote them left.
///
/// The relay reads the tree only once the member that opened its channel let
/// the relay's device in: the member is linked to the relay directly, and
/// hears the relay's access request on that link.
#[test]
fn a_file_the_relay_kept_reaches_a_device_that_joins_after_its_writer_left() {
    // The device that joins last: a runtime of its own that joins the
    // workspace and holds its files until the test stops it.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=debug,tilo_runtime=debug,tilo_relay=debug")
        .with_writer(std::io::stderr)
        .try_init();
    if let Some(invitation) = std::env::var_os(JOINER_INVITATION) {
        let profile = isolated_root(KEPT_TEST).unwrap();
        let device = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
        device.joiner().unwrap()(invitation.to_str().unwrap()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let Some(profile) = isolated_root(KEPT_TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();

    // The member that writes the files: the runtime that takes the
    // workspace's work.
    let member = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, member_device) =
        tilo_identity_host::broker::trusted_current_device(member.workspace()).unwrap();

    // The relay governs the workspace; its operator's desk owns it.
    let operator = SigningKey::from_bytes(&[51; 32]);
    let desk = certificate(
        &operator,
        &SigningKey::from_bytes(&[53; 32]),
        DeviceCapabilities::primary(),
    );
    let state = scenario.join("relay");
    let relay = run(relay(&state, &operator));
    let workspaces = relay.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(&created.workspace, &desk)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();

    // The member joins and holds the workspace's files at a folder.
    let joined = member.joiner().unwrap()(&run(invitation(
        &relay,
        &workspace,
        &desk,
        Some(member_device.user_public_key),
        1,
    )))
    .unwrap();
    assert_eq!(joined, workspace);
    let member_folder = member
        .joined_files_folder(&workspace)
        .expect("the member holds the joined workspace's files");

    // A command in the workspace's VM writes into the member's folder: a file
    // kept as loose blobs, and one large enough to be kept as chunks.
    let report = content(11, 300_000);
    let build = content(12, 3 * 1024 * 1024);
    std::fs::create_dir_all(member_folder.join("out")).unwrap();
    std::fs::write(member_folder.join("report.txt"), &report).unwrap();
    std::fs::write(member_folder.join("out").join("build.bin"), &build).unwrap();

    // The relay keeps the content of both while the member that wrote them is
    // still here: the member leaves entirely before anything reads them again.
    let kept = state
        .join("state")
        .join(tilo_relay::content::DIRECTORY)
        .join(&workspace);
    let written = u64::try_from(report.len() + build.len()).unwrap();
    wait_for_kept(
        &kept,
        written,
        "the content of the files the member wrote, on the relay",
    );
    drop(member);

    // The device that joins afterwards is given the same bytes, from the relay
    // alone: nothing else keeps a copy.
    let joiner_root = scenario.join("joiner");
    std::fs::create_dir_all(&joiner_root).unwrap();
    let log = joiner_root.join("device.log");
    let mut later = isolated_command(KEPT_TEST, &joiner_root)
        .env(
            JOINER_INVITATION,
            run(invitation(&relay, &workspace, &desk, None, 2)),
        )
        .env(
            tilo_test_env::hang::HANG_BOUND_ENV,
            super::super::super::tests::helper_hang_bound(),
        )
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::fs::File::create(joiner_root.join("device.err")).unwrap())
        .spawn_locked()
        .unwrap();
    let joiner_folder: PathBuf = isolated_home(&joiner_root)
        .join("Tilo")
        .join(WORKSPACE_NAME);
    wait_for_file(
        &joiner_folder.join("report.txt"),
        &report,
        "the file the relay kept, on the device that joined after its writer left",
        &log,
    );
    wait_for_file(
        &joiner_folder.join("out").join("build.bin"),
        &build,
        "the large file the relay kept, on the device that joined after its writer left",
        &log,
    );

    let _ = later.kill();
    let _ = later.wait();
    run(relay.shutdown());
}

/// A member that was away longer than the relay keeps content nothing names
/// comes back with a file whose bytes the relay let go of in the meantime:
/// the relay asks the writer for them again, as for any content a record
/// names and the relay lacks, and keeps them once more.
#[test]
fn content_the_relay_let_go_of_comes_back_from_the_writer_that_was_away() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=debug,tilo_runtime=debug,tilo_relay=debug")
        .with_writer(std::io::stderr)
        .try_init();
    let Some(profile) = isolated_root(RETURN_TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();
    let member = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, member_device) =
        tilo_identity_host::broker::trusted_current_device(member.workspace()).unwrap();
    let operator = SigningKey::from_bytes(&[61; 32]);
    let desk = certificate(
        &operator,
        &SigningKey::from_bytes(&[63; 32]),
        DeviceCapabilities::primary(),
    );
    let state = scenario.join("relay");
    let relay = run(relay(&state, &operator));
    let workspaces = relay.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(&created.workspace, &desk)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();
    let joined = member.joiner().unwrap()(&run(invitation(
        &relay,
        &workspace,
        &desk,
        Some(member_device.user_public_key),
        1,
    )))
    .unwrap();
    assert_eq!(joined, workspace);
    let member_folder = member
        .joined_files_folder(&workspace)
        .expect("the member holds the joined workspace's files");
    // The member goes away.
    drop(member);

    // The relay held the bytes of the member's draft, which no record named
    // yet; the member is away for longer than the relay keeps such content,
    // and the relay lets go of it.
    let draft = content(21, 300_000);
    let (manifest, _) = tilo_fs::content::build(&draft, None);
    let store = relay.content().get(&workspace).unwrap();
    let id = store.put(&manifest.encode()).unwrap();
    let chunks: Vec<String> = (0..manifest.chunks().len())
        .map(|index| {
            let range = manifest.chunk_range(index).unwrap();
            let range = usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap();
            store.put(&draft[range]).unwrap()
        })
        .collect();
    let past_the_grace = std::time::SystemTime::now() + Duration::from_secs(60);
    let collected = run(tilo_relay::content::collection::collect(
        workspaces,
        &relay.content(),
        &workspace,
        past_the_grace,
    ))
    .expect("every record of the workspace reads whole");
    assert!(
        collected.hashes.contains(&id),
        "nothing named the draft, so the relay let go of it: {collected:?}"
    );
    assert!(!store.has(&id));

    // Away, the member saved the draft into the workspace's files; it comes
    // back, and its tree names the draft.
    std::fs::write(member_folder.join("draft.txt"), &draft).unwrap();
    let member = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();

    // The relay asks the writer for the draft and keeps it again.
    let started = std::time::Instant::now();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    while !(store.has(&id) && chunks.iter().all(|chunk| store.has(chunk))) {
        assert!(
            started.elapsed() <= bound,
            "the relay never took the draft back from its writer: manifest kept {}, {} of {} chunks",
            store.has(&id),
            chunks.iter().filter(|chunk| store.has(chunk)).count(),
            chunks.len(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(member);
    run(relay.shutdown());
}
