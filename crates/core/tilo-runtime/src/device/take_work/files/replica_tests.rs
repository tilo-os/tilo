//! A workspace a server governs, its files held by the member that takes the
//! workspace's work and by a person's device, each a runtime of its own; and
//! the same files read by a device that joins once the member that wrote them
//! has left, from what the server kept of them.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the server, the runtimes and the person's device live for the whole scenario"
)]

use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::{isolated_command, isolated_home, isolated_root};
use ed25519_dalek::SigningKey;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tilo_peer_state::join::Role;
use tilo_server::workspaces::IssueInvitationRequest;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

const TEST: &str = "device::take_work::files::replica::tests::\
    a_file_made_on_the_vm_host_reaches_a_persons_device_and_their_edit_reaches_the_vm_host";
/// The invitation the person's device redeems, in its own process.
const PERSON_INVITATION: &str = "TILO_REPLICA_TEST_PERSON_INVITATION";
const KEPT_TEST: &str = "device::take_work::files::replica::tests::\
    a_file_the_server_kept_reaches_a_device_that_joins_after_its_writer_left";
const RETURN_TEST: &str = "device::take_work::files::replica::tests::\
    content_the_server_let_go_of_comes_back_from_the_writer_that_was_away";
const FOUNDED_TEST: &str = "device::take_work::files::replica::tests::\
    a_computer_with_no_login_keeps_its_files_in_the_workspace_founded_on_it";
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

/// Join `device` to the workspace `invitation` names, holding its files from
/// the join on as the runtime does for the workspaces a profile joined.
/// Answers the joined workspace's id.
fn join(device: &Daemon, invitation: &str) -> String {
    let workspaces = std::sync::Arc::clone(device.serving());
    let (workspace, name) = run(async move {
        let workspace = Box::pin(workspaces.copies().join_member(invitation)).await?;
        let name = workspaces
            .copies()
            .member_copy(workspace)
            .await
            .map_or_else(|| "Workspace".to_owned(), |(name, _)| name);
        Ok::<_, String>((workspace, name))
    })
    .unwrap();
    if let Some(replicas) = device.replicas() {
        let _ = crate::device::services::hold_files(replicas, &name, workspace);
    }
    workspace.to_string()
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

/// The server tilo.sh runs: a certified host of its operator, governing the
/// workspaces it creates.
async fn server(state: &Path, operator: &SigningKey) -> tilo_server::Server {
    let key = SigningKey::from_bytes(&tilo_test_env::node_key());
    // It owns the workspaces it governs, so it takes part in their channels
    // and their records are sealed.
    let cert = certificate(
        operator,
        &key,
        DeviceCapabilities {
            can_own_workspaces: true,
            ..DeviceCapabilities::member()
        },
    );
    // On the endpoint the other tests here dial through: a server handed an
    // endpoint waits for it to name where it is reached, which an endpoint
    // naming nowhere holds for the whole wait.
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&key.to_bytes())).await;
    tilo_server::Server::start_on_endpoint_with_protocols(
        tilo_server::ServerConfig {
            state_root: state.join("state"),
            identity_root: state.join("identity"),
            ..Default::default()
        },
        tilo_server::daemon::ServerIdentity {
            user_public_key: operator.verifying_key().to_bytes(),
            device_signing_key: key,
            device_cert: cert,
            revocations: RevocationList::new(),
        },
        endpoint,
        tilo_server::Reaching::OnItsOwn,
        Vec::new(),
    )
    .await
    .unwrap()
}

/// An editor's invitation to `workspace`, as text, that the operator's own
/// device `shell` issues; for anyone's device when `person` is `None`.
async fn invitation(
    server: &tilo_server::Server,
    workspace: &str,
    shell: &DeviceCert,
    person: Option<[u8; 32]>,
    request: u8,
) -> String {
    let issued = server
        .workspaces()
        .unwrap()
        .issue(
            workspace,
            shell,
            IssueInvitationRequest {
                request_id: [request; 32],
                role: Role::Editor,
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

/// Wait until the server's blobs under `dir` take `bytes` together, failing
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
            "{what}: the server keeps {kept} bytes of them under {}",
            dir.display(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Bytes no two calls with different `seed`s share, `length` of them, with no
/// stretch of them repeating: content is kept by its hash, so a file whose
/// chunks repeat takes less workspace than its length wherever it is kept.
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

/// A joined workspace's folder is a FUSE mount; the real-fuse check requires it.
macro_rules! require_fuse {
    () => {
        tilo_test_env::require_capability!(
            Fuse,
            tilo_fs::fuse_session::mountable(
                Path::new("/dev/fuse"),
                std::env::var_os("PATH").as_deref()
            ),
            "a joined workspace's folder is a FUSE mount"
        );
    };
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one file, from the host that makes it to the edit that returns, read top to bottom"
)]
fn a_file_made_on_the_vm_host_reaches_a_persons_device_and_their_edit_reaches_the_vm_host() {
    require_fuse!();
    // The person's device: a runtime of its own that joins the workspace and
    // holds its files until the test stops it.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=debug,tilo_runtime=debug,tilo_workspace_keys=debug")
        .with_writer(std::io::stderr)
        .try_init();
    if let Some(invitation) = std::env::var_os(PERSON_INVITATION) {
        let profile = isolated_root(TEST).unwrap();
        let device = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
        join(&device, invitation.to_str().unwrap());
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let Some(profile) = isolated_root(TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();

    // The VM host's member: the runtime that takes the workspace's work.
    let member = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, dev) =
        tilo_identity::trusted_device::current(&member.workspace().expect("the workspace"))
            .unwrap();

    // The server governs the workspace; its operator's shell owns it.
    let operator = SigningKey::from_bytes(&[41; 32]);
    let shell = certificate(
        &operator,
        &SigningKey::from_bytes(&[43; 32]),
        DeviceCapabilities::primary(),
    );
    let server = run(server(&scenario.join("server"), &operator));
    let workspaces = server.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(
                &created.workspace,
                (&shell, &SigningKey::from_bytes(&[43; 32])),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();

    // The member joins and holds the workspace's files at a folder.
    let issued = run(invitation(
        &server,
        &workspace,
        &shell,
        Some(dev.user_public_key),
        1,
    ));
    let joined = join(&member, &issued);
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
            run(invitation(&server, &workspace, &shell, None, 2)),
        )
        .env(
            tilo_test_env::hang::HANG_BOUND_ENV,
            crate::device::services::tests::helper_hang_bound(),
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
    run(server.shutdown());
}

/// Keep this process's logs at the channels these tests read.
fn keep_logs() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=debug,tilo_runtime=debug,tilo_server=debug")
        .with_writer(std::io::stderr)
        .try_init();
}

/// In the helper process the test starts: join the workspace and hold its files until the test
/// stops this process.
fn join_as_the_device_that_joins_last(invitation: &std::ffi::OsStr) {
    let profile = isolated_root(KEPT_TEST).unwrap();
    let device = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    join(&device, invitation.to_str().unwrap());
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The files a server keeps reach a device that joins after the member that
/// wrote them left.
///
/// The server reads the tree only once the member that opened its channel let
/// the server's device in: the member is linked to the server directly, and
/// hears the server's access request on that link.
#[test]
fn a_file_the_server_kept_reaches_a_device_that_joins_after_its_writer_left() {
    require_fuse!();
    // The device that joins last: a runtime of its own that joins the
    // workspace and holds its files until the test stops it.
    keep_logs();
    if let Some(invitation) = std::env::var_os(JOINER_INVITATION) {
        join_as_the_device_that_joins_last(&invitation);
    }
    let Some(profile) = isolated_root(KEPT_TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();

    // The member that writes the files: the runtime that takes the
    // workspace's work.
    let member = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, member_device) =
        tilo_identity::trusted_device::current(&member.workspace().unwrap()).unwrap();

    // The server governs the workspace; its operator's shell owns it.
    let operator = SigningKey::from_bytes(&[51; 32]);
    let shell = certificate(
        &operator,
        &SigningKey::from_bytes(&[53; 32]),
        DeviceCapabilities::primary(),
    );
    let state = scenario.join("server");
    let server = run(server(&state, &operator));
    let workspaces = server.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(
                &created.workspace,
                (&shell, &SigningKey::from_bytes(&[53; 32])),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();

    // The member joins and holds the workspace's files at a folder.
    let issued = run(invitation(
        &server,
        &workspace,
        &shell,
        Some(member_device.user_public_key),
        1,
    ));
    let joined = join(&member, &issued);
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

    // The server keeps the content of both while the member that wrote them is
    // still here: the member leaves entirely before anything reads them again.
    let kept = state
        .join("state")
        .join(tilo_server::content::DIRECTORY)
        .join(&workspace);
    let written = u64::try_from(report.len() + build.len()).unwrap();
    wait_for_kept(
        &kept,
        written,
        "the content of the files the member wrote, on the server",
    );
    drop(member);

    // The device that joins afterwards is given the same bytes, from the server
    // alone: nothing else keeps a copy.
    let joiner_root = scenario.join("joiner");
    std::fs::create_dir_all(&joiner_root).unwrap();
    let log = joiner_root.join("device.log");
    let mut later = isolated_command(KEPT_TEST, &joiner_root)
        .env(
            JOINER_INVITATION,
            run(invitation(&server, &workspace, &shell, None, 2)),
        )
        .env(
            tilo_test_env::hang::HANG_BOUND_ENV,
            crate::device::services::tests::helper_hang_bound(),
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
        "the file the server kept, on the device that joined after its writer left",
        &log,
    );
    wait_for_file(
        &joiner_folder.join("out").join("build.bin"),
        &build,
        "the large file the server kept, on the device that joined after its writer left",
        &log,
    );

    let _ = later.kill();
    let _ = later.wait();
    run(server.shutdown());
}

/// A member that comes back to a server lacking the bytes of a file its
/// tree names, ones the server let go of while the member was away (a store
/// put back from an older copy, content collected while nothing named it
/// yet), gives them to the server again: the server asks the writer for
/// them, as for any content a record names and the server lacks, and keeps
/// them once more.
#[test]
fn content_the_server_let_go_of_comes_back_from_the_writer_that_was_away() {
    require_fuse!();
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=debug,tilo_runtime=debug,tilo_server=debug")
        .with_writer(std::io::stderr)
        .try_init();
    let Some(profile) = isolated_root(RETURN_TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();
    let member = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, member_device) =
        tilo_identity::trusted_device::current(&member.workspace().unwrap()).unwrap();
    let operator = SigningKey::from_bytes(&[61; 32]);
    let shell = certificate(
        &operator,
        &SigningKey::from_bytes(&[63; 32]),
        DeviceCapabilities::primary(),
    );
    let state = scenario.join("server");
    let server = run(server(&state, &operator));
    let workspaces = server.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(
                &created.workspace,
                (&shell, &SigningKey::from_bytes(&[63; 32])),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();
    let issued = run(invitation(
        &server,
        &workspace,
        &shell,
        Some(member_device.user_public_key),
        1,
    ));
    let joined = join(&member, &issued);
    assert_eq!(joined, workspace);
    let member_folder = member
        .joined_files_folder(&workspace)
        .expect("the member holds the joined workspace's files");

    // The member saves a draft, large enough to be kept as chunks, and the
    // server keeps its bytes.
    let draft = content(21, 300_000);
    std::fs::write(member_folder.join("draft.txt"), &draft).unwrap();
    let kept = state
        .join("state")
        .join(tilo_server::content::DIRECTORY)
        .join(&workspace);
    let length = u64::try_from(draft.len()).unwrap();
    wait_for_kept(&kept, length, "the draft the member wrote, on the server");

    // The member goes away, and the server lets go of the draft's bytes.
    drop(member);
    let past_the_grace = std::time::SystemTime::now() + Duration::from_secs(60);
    let collected = server
        .content()
        .collect(
            &workspace,
            &std::collections::BTreeSet::new(),
            past_the_grace,
        )
        .unwrap();
    assert!(
        !collected.hashes.is_empty() && bytes_under(&kept) < length,
        "the server let go of the draft: {collected:?}, {} bytes kept",
        bytes_under(&kept),
    );

    // The member comes back; the server asks it for the draft its tree names
    // and keeps it again. The member's folder says it is in sync only once
    // the server says it lacks none of the files: until then the draft is on
    // the member alone, and wiping it would lose it.
    let member = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    wait_for_kept(
        &kept,
        length,
        "the draft the server let go of, taken back from its writer",
    );
    let back = std::time::Instant::now();
    loop {
        let status = member.joined_files_status(&workspace);
        if status.as_ref().is_some_and(|status| {
            status.state == tilo_fs::tree_sync::FolderState::InSync && status.host_lacks == Some(0)
        }) {
            break;
        }
        assert!(
            back.elapsed() <= tilo_test_env::hang_bound(),
            "the member's folder never says it is in sync, with the server's word that it lacks \
             none of the files, once the server keeps the draft: {status:?}",
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(member);
    run(server.shutdown());
}

const CLONE_TEST: &str = "device::take_work::files::replica::tests::\
    a_repository_cloned_into_the_vm_hosts_folder_reaches_a_persons_device_whole";

/// Writes a repository into `folder` as `git clone` does: the pack grows in a
/// temporary file renamed into place once whole, its index likewise, then
/// more worktree files land at once than one folder pass carries, and git's
/// index replaces its lock file. Answers every file it leaves, with its bytes.
fn clone_like_git(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    use std::io::Write as _;

    let repo = folder.join("repo");
    let pack_dir = repo.join(".git").join("objects").join("pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    let mut files = vec![(
        repo.join(".git").join("HEAD"),
        b"ref: refs/heads/main\n".to_vec(),
    )];
    std::fs::write(&files[0].0, &files[0].1).unwrap();
    // The pack, written a step at a time while the folder's passes see it.
    let temporary = pack_dir.join("tmp_pack_x0Ab9z");
    let mut pack = Vec::new();
    {
        let mut file = std::fs::File::create(&temporary).unwrap();
        for step in 0..8_u32 {
            let part = content(step.saturating_add(100), 1024 * 1024);
            file.write_all(&part).unwrap();
            file.flush().unwrap();
            pack.extend_from_slice(&part);
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    let packed = pack_dir.join("pack-5f0c.pack");
    std::fs::rename(&temporary, &packed).unwrap();
    files.push((packed, pack));
    let index = content(200, 200_000);
    std::fs::write(pack_dir.join("tmp_idx_q1"), &index).unwrap();
    let indexed = pack_dir.join("pack-5f0c.idx");
    std::fs::rename(pack_dir.join("tmp_idx_q1"), &indexed).unwrap();
    files.push((indexed, index));
    // The worktree: more small files than one pass carries, across many
    // directories.
    for file in 0..700_u32 {
        let dir = repo.join(format!("crates/c{}/src", file % 30));
        std::fs::create_dir_all(&dir).unwrap();
        let length = usize::try_from(file % 7).unwrap().saturating_mul(300);
        let bytes = content(file.saturating_add(1000), length.saturating_add(600));
        let path = dir.join(format!("m{file}.rs"));
        std::fs::write(&path, &bytes).unwrap();
        files.push((path, bytes));
    }
    // The index, as git writes it: a lock file renamed over it.
    let staged = content(300, 150_000);
    let lock = repo.join(".git").join("index.lock");
    std::fs::write(&lock, &staged).unwrap();
    std::fs::rename(&lock, repo.join(".git").join("index")).unwrap();
    files.push((repo.join(".git").join("index"), staged));
    files
}

/// Wait until `person_folder` holds every file of `cloned`, made in
/// `member_folder`, failing with what the member (its log at `said`) and the
/// person's device (under `person_root`) said.
fn wait_for_clone(
    cloned: &[(PathBuf, Vec<u8>)],
    member_folder: &Path,
    person_folder: &Path,
    said: &Path,
    person_root: &Path,
) {
    let started = std::time::Instant::now();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    loop {
        let missing: Vec<&Path> = cloned
            .iter()
            .filter(|(path, bytes)| {
                let at = person_folder.join(path.strip_prefix(member_folder).unwrap());
                std::fs::read(at).ok().as_ref() != Some(bytes)
            })
            .map(|(path, _)| path.as_path())
            .collect();
        if missing.is_empty() {
            return;
        }
        assert!(
            started.elapsed() <= bound,
            "{} of the {} files cloned on the VM host are not on the person's device, {:?} \
             among them\nthe member:\n{}\nthe person's device:\n{}",
            missing.len(),
            cloned.len(),
            missing.first(),
            std::fs::read_to_string(said).unwrap_or_default(),
            std::fs::read_to_string(person_root.join("device.err")).unwrap_or_default(),
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A repository cloned into the VM host's folder reaches a person's device
/// whole, the member it was cloned on taking part in the workspace all along:
/// its changes are never refused as if the workspace were no longer held.
#[test]
fn a_repository_cloned_into_the_vm_hosts_folder_reaches_a_persons_device_whole() {
    require_fuse!();
    if let Some(invitation) = std::env::var_os(PERSON_INVITATION) {
        let profile = isolated_root(CLONE_TEST).unwrap();
        let device = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
        join(&device, invitation.to_str().unwrap());
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    // The member, in the process `isolated_root` starts, says what it does
    // into a file the test reads.
    if let Some(profile) = std::env::var_os(crate::device::services::tests::PROCESS_ROOT) {
        let file = std::fs::File::create(PathBuf::from(profile).with_extension("log")).unwrap();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("warn,tilo_fs=info,tilo_runtime=debug")
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .try_init();
    }
    let Some(profile) = isolated_root(CLONE_TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();
    let said = profile.with_extension("log");
    let member = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, member_device) =
        tilo_identity::trusted_device::current(&member.workspace().unwrap()).unwrap();
    let operator = SigningKey::from_bytes(&[71; 32]);
    let shell = certificate(
        &operator,
        &SigningKey::from_bytes(&[73; 32]),
        DeviceCapabilities::primary(),
    );
    let server = run(server(&scenario.join("server"), &operator));
    let workspaces = server.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(
                &created.workspace,
                (&shell, &SigningKey::from_bytes(&[73; 32])),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();
    let issued = run(invitation(
        &server,
        &workspace,
        &shell,
        Some(member_device.user_public_key),
        1,
    ));
    let joined = join(&member, &issued);
    assert_eq!(joined, workspace);
    let member_folder = member
        .joined_files_folder(&workspace)
        .expect("the member holds the joined workspace's files");

    let person_root = scenario.join("person");
    std::fs::create_dir_all(&person_root).unwrap();
    let log = person_root.join("device.log");
    let mut person = isolated_command(CLONE_TEST, &person_root)
        .env(
            PERSON_INVITATION,
            run(invitation(&server, &workspace, &shell, None, 2)),
        )
        .env(
            tilo_test_env::hang::HANG_BOUND_ENV,
            crate::device::services::tests::helper_hang_bound(),
        )
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::fs::File::create(person_root.join("device.err")).unwrap())
        .spawn_locked()
        .unwrap();
    let person_folder: PathBuf = isolated_home(&person_root)
        .join("Tilo")
        .join(WORKSPACE_NAME);

    let cloned = clone_like_git(&member_folder);
    wait_for_clone(&cloned, &member_folder, &person_folder, &said, &person_root);
    let said = std::fs::read_to_string(&said).unwrap_or_default();
    assert!(
        !said.contains("no longer hosted"),
        "the member's own changes were refused:\n{said}"
    );
    let _ = person.kill();
    let _ = person.wait();
    drop(member);
    run(server.shutdown());
}

const WIPED_TEST: &str = "device::take_work::files::replica::tests::\
    a_member_whose_disk_was_wiped_gets_the_workspaces_files_back";
/// The invitation the member redeems, in its own process, each time it starts.
const MEMBER_INVITATION: &str = "TILO_REPLICA_TEST_MEMBER_INVITATION";

/// The FUSE mounts still up under `root`: the member's folders, its own workspace's among them.
fn mounts_under(root: &Path) -> Vec<String> {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return Vec::new();
    };
    let under = format!("{}/", root.to_string_lossy().replace(' ', "\\040"));
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _source = fields.next();
            let point = fields.next()?;
            let kind = fields.next()?;
            (point.starts_with(&under) && kind.starts_with("fuse")).then(|| point.to_owned())
        })
        .collect()
}

/// A member killed the hard way leaves its mounts to the guardian: wait for them all to go
/// before wiping, since a folder still mounted cannot be removed (ENOTCONN).
fn wait_for_unmount(root: &Path) {
    let cleared = std::time::Instant::now();
    loop {
        let left = mounts_under(root);
        if left.is_empty() {
            return;
        }
        assert!(
            cleared.elapsed() <= Duration::from_secs(30),
            "the stopped member's mounts never went: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Empties the disk of the device whose process runs under `root`, as a VM
/// host moved to a new disk finds it: its profile, with every workspace it
/// held, and its home, with every folder. What is left is its identity and the
/// device settings that name the relay the test reaches.
fn wipe_all_but_identity(root: &Path) {
    let environment = root.join("environment");
    for gone in [
        root.join("profile"),
        environment.join("home"),
        environment.join("state"),
        environment.join("cache"),
    ] {
        if gone.exists() {
            std::fs::remove_dir_all(&gone).unwrap_or_else(|error| {
                let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
                panic!(
                    "could not wipe {}: {error}; mounted:\n{mounts}",
                    gone.display()
                )
            });
        }
        std::fs::create_dir_all(&gone).unwrap();
    }
}

/// The member's process: this device joins with `invitation` and holds the
/// workspace's files until the test stops it.
fn serve_as_member(invitation: &std::ffi::OsStr) -> ! {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_fs=info,tilo_runtime=info")
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
    let profile = isolated_root(WIPED_TEST).unwrap();
    let device = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    join(&device, invitation.to_str().unwrap());
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Writes small files across folders under `folder`, and a file large enough
/// to be kept as chunks. Answers every file it wrote, with its bytes.
fn write_tree(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for file in 0..150_u32 {
        let dir = folder.join(format!("src/m{}", file % 12));
        std::fs::create_dir_all(&dir).unwrap();
        let length = usize::try_from(file % 5).unwrap().saturating_mul(400);
        let bytes = content(file.saturating_add(2000), length.saturating_add(500));
        let path = dir.join(format!("f{file}.rs"));
        std::fs::write(&path, &bytes).unwrap();
        files.push((path, bytes));
    }
    let large = content(2999, 3 * 1024 * 1024);
    std::fs::write(folder.join("build.bin"), &large).unwrap();
    files.push((folder.join("build.bin"), large));
    files
}

/// Wait until every file of `files` holds its bytes, failing, once the test
/// that `started` then has run as long as it may, with how many do not, what
/// `folder` holds, and what the member `said`.
fn wait_for_files(
    files: &[(PathBuf, Vec<u8>)],
    folder: &Path,
    started: std::time::Instant,
    said: &dyn Fn() -> String,
) {
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    loop {
        let missing: Vec<&Path> = files
            .iter()
            .filter(|(path, bytes)| std::fs::read(path).ok().as_ref() != Some(bytes))
            .map(|(path, _)| path.as_path())
            .collect();
        if missing.is_empty() {
            return;
        }
        assert!(
            started.elapsed() <= bound,
            "{} of the {} files the workspace holds are not in the wiped member's folder, {:?} \
             among them; the folder holds {} bytes\nthe member:\n{}",
            missing.len(),
            files.len(),
            missing.first(),
            bytes_under(folder),
            said(),
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Wait until the server keeps every file written on the member: all their
/// bytes, a member's hold on each, and nothing of them still arriving.
fn wait_until_kept(
    server: &tilo_server::Server,
    workspace: &str,
    kept: &Path,
    written_files: &[(PathBuf, Vec<u8>)],
    started: std::time::Instant,
    said: &dyn Fn() -> String,
) {
    let written = written_files
        .iter()
        .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap())
        .fold(0, u64::saturating_add);
    // Sealing and manifests add bytes: the bytes alone may add up while a few
    // small files are still on the member alone, which the wipe would lose.
    let workspaces = server.workspaces().unwrap();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    loop {
        let held = workspaces.content_held(workspace);
        if bytes_under(kept) >= written && held.held >= written_files.len() && held.going == 0 {
            break;
        }
        assert!(
            started.elapsed() <= bound,
            "the server keeps {} of the {written} bytes written on the member, holding {} of \
             the {} files with {} still arriving:\n{}",
            bytes_under(kept),
            held.held,
            written_files.len(),
            held.going,
            said(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A member whose disk was wiped, a VM host that moved to a new disk with its
/// identity alone, holds the workspace's files again in its folder: every
/// file the workspace holds, with its bytes, though nothing on its disk
/// remembers them and the workspace's tree names this very device as the
/// writer of each.
#[test]
fn a_member_whose_disk_was_wiped_gets_the_workspaces_files_back() {
    require_fuse!();
    if let Some(invitation) = std::env::var_os(MEMBER_INVITATION) {
        serve_as_member(&invitation);
    }
    let Some(profile) = isolated_root(WIPED_TEST) else {
        return;
    };
    let scenario = profile.parent().unwrap().to_path_buf();
    let operator = SigningKey::from_bytes(&[81; 32]);
    let shell = certificate(
        &operator,
        &SigningKey::from_bytes(&[83; 32]),
        DeviceCapabilities::primary(),
    );
    let state = scenario.join("server");
    let server = run(server(&state, &operator));
    let workspaces = server.workspaces().unwrap();
    let workspace = run(async {
        let created = workspaces.create(WORKSPACE_NAME).await?;
        workspaces
            .enter(
                &created.workspace,
                (&shell, &SigningKey::from_bytes(&[83; 32])),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(created.workspace)
    })
    .unwrap();

    // The member, a device of its own, joins and holds the workspace's files.
    let member_root = scenario.join("member");
    std::fs::create_dir_all(&member_root).unwrap();
    let start = |request: u8, run_name: &str| {
        isolated_command(WIPED_TEST, &member_root)
            .env(
                MEMBER_INVITATION,
                run(invitation(&server, &workspace, &shell, None, request)),
            )
            .env(
                tilo_test_env::hang::HANG_BOUND_ENV,
                crate::device::services::tests::helper_hang_bound(),
            )
            .stdout(std::fs::File::create(member_root.join(format!("{run_name}.log"))).unwrap())
            .stderr(std::fs::File::create(member_root.join(format!("{run_name}.err"))).unwrap())
            .spawn_locked()
            .unwrap()
    };
    let said = |run_name: &str| {
        std::fs::read_to_string(member_root.join(format!("{run_name}.err"))).unwrap_or_default()
    };
    let folder = isolated_home(&member_root)
        .join("Tilo")
        .join(WORKSPACE_NAME);
    let mut first = start(1, "first");
    let started = std::time::Instant::now();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    while !folder.is_dir() {
        assert!(
            started.elapsed() <= bound,
            "the member never held the workspace's files:\n{}",
            said("first")
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // A tree of files written on the member reaches the server whole.
    let written_files = write_tree(&folder);
    let kept = state
        .join("state")
        .join(tilo_server::content::DIRECTORY)
        .join(&workspace);
    wait_until_kept(&server, &workspace, &kept, &written_files, started, &|| {
        said("first")
    });
    let _ = first.kill();
    let _ = first.wait();
    eprintln!("the server kept the tree after {:?}", started.elapsed());

    wait_for_unmount(&member_root);

    // The member's disk is wiped; it starts again with its identity alone and
    // takes part in the workspace again.
    wipe_all_but_identity(&member_root);
    let mut second = start(2, "second");
    wait_for_files(&written_files, &folder, started, &|| said("second"));
    let _ = second.kill();
    let _ = second.wait();
    run(server.shutdown());
}

/// The Files tree the workspace founded on `daemon` keeps, its own record and every chunk it
/// lists: `None` until it keeps the tree's record.
fn kept_tree(daemon: &Daemon) -> Option<fs_protocol::tree::Tree> {
    let workspace = collab_types::WorkspaceId::parse(&daemon.workspace().ok()?)?;
    let read = |record: &str| {
        daemon
            .serving()
            .test_kept_document(workspace, tilo_fs::tree_sync::FILES_APP, record)
            .ok()
    };
    let index = read(&fs_protocol::tree::document_id())?;
    let listed = fs_protocol::tree::listed_chunks(&automerge::AutoCommit::load(&index).ok()?);
    let chunks: Vec<Vec<u8>> = listed
        .into_iter()
        .filter(|chunk| *chunk != 0)
        .filter_map(|chunk| read(&fs_protocol::tree::chunk_document_id(chunk)))
        .collect();
    fs_protocol::tree::Tree::of_records(Some(&index), chunks.iter().map(Vec::as_slice)).ok()
}

/// Wait until `daemon`'s workspace keeps, in its Files tree, a file of `size` bytes with its
/// content named.
fn wait_for_kept_file(daemon: &Daemon, size: usize, what: &str) {
    let size = u64::try_from(size).unwrap();
    let started = std::time::Instant::now();
    let bound = tilo_test_env::hang_bound().saturating_sub(WAIT_LEAD);
    loop {
        let tree = kept_tree(daemon);
        if tree.as_ref().is_some_and(|tree| {
            tree.nodes
                .values()
                .any(|node| node.size == size && node.hash.is_some())
        }) {
            return;
        }
        assert!(
            started.elapsed() <= bound,
            "{what}: the tree holds {tree:?}; the folder's sync says {:?}",
            daemon.joined_files_status(&daemon.workspace().unwrap())
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A computer with no login keeps the person's files in the workspace founded on it, as Tilo
/// with no server does: its folder shows them, and a file written there is the workspace's, so
/// it comes back though the folder and everything the folder's sync kept are gone.
#[test]
fn a_computer_with_no_login_keeps_its_files_in_the_workspace_founded_on_it() {
    require_fuse!();
    let Some(profile) = isolated_root(FOUNDED_TEST) else {
        return;
    };
    let folder = isolated_home(profile.parent().unwrap())
        .join("Tilo")
        .join("Home");
    let written = content(77, 200_000);
    {
        let daemon = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
        assert_eq!(
            daemon.joined_files_folder(&daemon.workspace().unwrap()),
            Some(folder.clone()),
            "the founded workspace's files are shown at its folder"
        );
        std::fs::write(folder.join("notes.bin"), &written).unwrap();
        wait_for_kept_file(
            &daemon,
            written.len(),
            "the file reaches the workspace's tree",
        );
    }
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::remove_dir_all(profile.join("files")).unwrap();
    let daemon = Daemon::start_reporting(&profile, &mut |_| {}).unwrap();
    wait_for_file(
        &folder.join("notes.bin"),
        &written,
        "the workspace's file is in its folder again",
        &profile.join("none.log"),
    );
    drop(daemon);
}
