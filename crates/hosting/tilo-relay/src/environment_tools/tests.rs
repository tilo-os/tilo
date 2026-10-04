//! Members' tool builds: the request path, the member's quota, and the key
//! staying with the worker.

use std::os::unix::fs::PermissionsExt;

use env_tools::delegated::RemoteBuilder;
use tilo_environment::ToolBuilder;

use super::*;

const SEED: [u8; 32] = [0x5A; 32];

/// The unprivileged half without dnf: seal a fixed tree for the
/// publisher, as `tilo-env-tools build-for` would after installing.
struct SealFixture;

impl MemberBuild for SealFixture {
    fn build(
        &self,
        _member: &MemberRun,
        request: &BuildRequest,
        publisher: [u8; 32],
        out: &Path,
        progress: &mut dyn FnMut(String),
    ) -> Result<(), String> {
        progress("installing hello\n".into());
        let tree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tree.path().join("usr/bin")).unwrap();
        std::fs::write(tree.path().join("usr/bin/hello"), "hi").unwrap();
        let key = env_tools::CacheKey::new(request.runtime.parse().unwrap(), ["hello-1-1.x86_64"]);
        let packed = tilo_app_package::seal_image_for(
            tree.path(),
            &env_tools::image::tool_spec(
                &key,
                &request.packages,
                env_tools::image::host_arch().unwrap(),
            ),
            publisher,
        )
        .unwrap();
        env_tools::image::write_tapp(out, &packed).map_err(|error| error.to_string())
    }
}

fn member(root: &Path) -> MemberRun {
    let area = MemberArea::of(root, &[3; 32]);
    std::fs::create_dir_all(area.runtime()).unwrap();
    MemberRun {
        // The tool service accepts its backend's environment; these tests do
        // not need the Linux guest launcher to supply that input.
        env: vec![
            ("HOME".into(), area.root().join("home").into_os_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ],
        area,
        uid: None,
        cgroup: None,
    }
}

fn walk(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(path).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_gets_a_relay_signed_tool_over_its_socket() {
    // macOS's default temporary directory leaves too little room for the
    // member subdirectories within a Unix socket address.
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let member = member(root.path());
    let service = ToolBuildService::new(
        ed25519_dalek::SigningKey::from_bytes(&SEED),
        Arc::new(SealFixture),
    );
    let listening = service.listen(member.clone()).unwrap();
    let runtime = PackageId::from_bytes([1; 32]);
    let socket = member.socket();
    let (id, progress) = tokio::task::spawn_blocking(move || {
        let mut progress = Vec::new();
        let id = RemoteBuilder::new(socket).build(&runtime, &["hello".into()], &mut progress);
        (id, progress)
    })
    .await
    .unwrap();
    let id = id.unwrap();
    assert_eq!(String::from_utf8(progress).unwrap(), "installing hello\n");

    // The signed image waits on the member's shelf and verifies against the
    // relay's key; nothing unsigned is left behind.
    let bytes = std::fs::read(member.shelf().join(format!("{}.tapp", id.to_hex()))).unwrap();
    let mut blobs = std::collections::BTreeMap::<tilo_app_package::BlobHash, Vec<u8>>::new();
    let read = tilo_app_package::tapp::read(bytes.as_slice(), &mut blobs).unwrap();
    assert!(
        tilo_app_package::verify(&read.meta, &[service.publisher()])
            .manifest()
            .is_some()
    );
    assert_eq!(std::fs::read_dir(member.builds()).unwrap().count(), 0);

    // The relay's secret is nowhere in the member's area.
    let mut files = Vec::new();
    walk(member.area.root(), &mut files);
    for file in files {
        let contents = std::fs::read(&file).unwrap();
        assert!(
            !contents.windows(SEED.len()).any(|window| window == SEED),
            "{} holds the relay key",
            file.display()
        );
    }

    // A package that reads as an option is refused before anything runs.
    let socket = member.socket();
    let refused = tokio::task::spawn_blocking(move || {
        RemoteBuilder::new(socket).build(&runtime, &["--installroot=/".into()], &mut Vec::new())
    })
    .await
    .unwrap();
    assert!(refused.is_err());
    listening.abort();
}

#[test]
fn a_build_runs_as_the_member_in_their_quota_without_the_key() {
    let root = tempfile::tempdir().unwrap();
    let cgroup = root.path().join("cgroup");
    std::fs::create_dir_all(&cgroup).unwrap();
    let mut member = member(root.path());
    member.cgroup = Some(cgroup.clone());
    let request = BuildRequest {
        runtime: PackageId::from_bytes([1; 32]).to_string(),
        packages: vec!["hello".into()],
    };
    // `true` stands in for `tilo-env-tools`: it starts, joins the quota,
    // and leaves no image, which fails the build after it ran.
    let build = EnvToolsBuild {
        binary: PathBuf::from("/usr/bin/true"),
    };
    let command = build.command(&member, &request, [9; 32], Path::new("/out.tapp"));
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        &args[args.len() - 2..],
        ["--", "hello"],
        "packages never read as options"
    );
    let envs: Vec<_> = command
        .get_envs()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();
    assert!(!envs.iter().any(|key| key == "TILO_IDENTITY_ROOT"));
    assert!(
        envs.iter().any(|key| key == "HOME"),
        "the member's own home"
    );

    let mut progress = |_| {};
    build
        .build(
            &member,
            &request,
            [9; 32],
            Path::new("/out.tapp"),
            &mut progress,
        )
        .unwrap();
    let placed = std::fs::read_to_string(cgroup.join("cgroup.procs")).unwrap();
    assert!(
        placed.trim().parse::<u32>().is_ok(),
        "the build joined the member's cgroup"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn the_supervisor_learns_the_relays_public_key_and_never_its_secret() {
    let root = tempfile::tempdir().unwrap();
    let mut member = member(root.path());
    member.env = crate::environment_guest::member_env(&member.area, &[]);
    let launcher = crate::environment_guest::UshGuestLauncher::new(
        crate::environment_guest::UshLauncherConfig::at(root.path().to_path_buf()),
    );
    let public = ed25519_dalek::SigningKey::from_bytes(&SEED)
        .verifying_key()
        .to_bytes();
    let command = launcher.supervisor_command(&member, Path::new("/ush.sock"), Some(public));
    let envs: std::collections::BTreeMap<String, String> = command
        .get_envs()
        .filter_map(|(key, value)| {
            Some((
                key.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    assert_eq!(
        envs.get(ush::images::TRUSTED_PUBLISHER_ENV),
        Some(&hex::encode(public))
    );
    assert!(envs.contains_key(env_tools::delegated::BUILDS_SOCKET_ENV));
    assert!(!envs.contains_key("TILO_IDENTITY_ROOT"));
    assert!(
        !envs
            .values()
            .any(|value| value.contains(&hex::encode(SEED))),
        "the secret is in no variable"
    );
}

#[test]
fn builds_are_on_only_while_the_key_file_is_the_relays_alone() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("keypair.bin");
    std::fs::write(&key, SEED).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(key_file_private(&key).unwrap());
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(!key_file_private(&key).unwrap(), "a group could read it");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o604)).unwrap();
    assert!(!key_file_private(&key).unwrap(), "anyone could read it");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&key, &link).unwrap();
    assert!(
        !key_file_private(&link).unwrap(),
        "a link is not the key file"
    );
}
