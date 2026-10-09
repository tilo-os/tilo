//! What a Seatbelt-confined app may touch, from the Linux launcher's policy
//! inputs.
//!
//! Nothing here is a macOS-specific policy decision. The inputs are the ones
//! `tilo-launcher` confines with on Linux:
//!
//! - the app's one writable directory, located by [`launcher::appdir`] (per
//!   room, per package) under `~/Library/Application Support/Tilo`, beside the
//!   runtime profile;
//! - the user's grants, already evaluated into a
//!   [`launcher::capabilities::CapabilityPlan`];
//! - the network outcome, already resolved into a
//!   [`launcher::netgate::EgressDecision`].
//!
//! Seatbelt matches the *resolved* path of every access (`/tmp` is
//! `/private/tmp`, `/var` is `/private/var`), so every path is canonicalised
//! here; an unresolved alias in a profile silently grants nothing, or denies
//! nothing.
//!
//! ## What does not map
//!
//! Recorded per launch in [`SeatbeltPolicy::not_enforced`] and documented in
//! `docs/app-model/sandbox.md` ("macOS: App Sandbox or Seatbelt"):
//!
//! - **Device grants.** Cameras and microphones sit behind TCC on macOS, not
//!   device nodes; the profile grants no TCC service, so a device grant is
//!   not honoured (the app is denied, never widened).
//! - **Filtered egress without a gate.** There is no network namespace to
//!   hide the host network behind, so a host allowlist is only reachable
//!   through the gate's loopback port
//!   ([`launcher::netgate::LoopbackGate`]). With no gate, the launch is
//!   offline.

use std::io;
use std::path::{Path, PathBuf};

use launcher::appdir;
use launcher::capabilities::CapabilityPlan;
use launcher::netgate::EgressDecision;

/// Where Tilo keeps per-user state on macOS, relative to `HOME`. The runtime
/// profile is `<this>/runtime`; app containers are `<this>/tilo-rooms/…`.
pub const TILO_SUPPORT_DIR: &str = "Library/Application Support/Tilo";

/// Subdirectory of the container used as `TMPDIR`: the per-user temporary
/// directory (`/var/folders/…`) is shared by every process of the user.
pub const CONTAINER_TMP: &str = "tmp";

/// What `libsystem` appends to the per-user cache and temporary directories
/// (`confstr(_CS_DARWIN_USER_CACHE_DIR)`), as launchd does for XPC services.
/// Metal keeps its compiled shaders below the cache directory, so this gives
/// each app its own shader cache.
pub const DARWIN_DIR_SUFFIX_ENV: &str = "DIRHELPER_USER_DIR_SUFFIX";

/// The Tilo data home for `home`.
pub fn data_home(home: &Path) -> PathBuf {
    home.join(TILO_SUPPORT_DIR)
}

/// The container of `package_id` in the room of `workspace_id`.
pub fn container_dir(data_home: &Path, workspace_id: Option<&str>, package_id: &str) -> PathBuf {
    appdir::host_app_dir(data_home, &appdir::room_slug(workspace_id), package_id)
}

/// Create the container (and its XDG tree and `tmp`) and return its path.
///
/// # Errors
///
/// Returns the error of creating any of the directories; an app with nowhere
/// to write loses the user's work, so the launch is refused.
pub fn prepare_container(
    data_home: &Path,
    workspace_id: Option<&str>,
    package_id: &str,
) -> io::Result<PathBuf> {
    let dir = appdir::prepare_host_app_dir(data_home, workspace_id, package_id)?;
    std::fs::create_dir_all(dir.join(CONTAINER_TMP))?;
    Ok(dir)
}

/// The [`DARWIN_DIR_SUFFIX_ENV`] of the app whose container is `container`:
/// one path component per room and package. `libsystem` accepts no `/` in
/// it, and `+` is outside the alphabet of both components, so two containers
/// never share a suffix.
pub fn darwin_dir_suffix(container: &Path) -> Option<String> {
    let package = container.file_name()?.to_str()?;
    let room = container.parent()?.file_name()?.to_str()?;
    Some(format!("so.tilo+{room}+{package}"))
}

/// The app's own directory below the per-user cache directory `user_cache`,
/// created private to the user: where Metal caches the app's compiled
/// shaders across launches.
///
/// # Errors
///
/// Returns the error of creating it; `InvalidInput` when `container` has no
/// room and package to name it after.
pub fn prepare_darwin_cache(user_cache: &Path, container: &Path) -> io::Result<DarwinCache> {
    use std::os::unix::fs::DirBuilderExt as _;
    let suffix = darwin_dir_suffix(container).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the container names no room and package",
        )
    })?;
    let dir = std::fs::canonicalize(user_cache)?.join(&suffix);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    // A link here would hand the app whatever it points at.
    if !std::fs::symlink_metadata(&dir)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the app's cache directory is not a directory",
        ));
    }
    Ok(DarwinCache { suffix, dir })
}

/// An app's own per-user cache directory: [`DARWIN_DIR_SUFFIX_ENV`] and
/// where it resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarwinCache {
    pub suffix: String,
    pub dir: PathBuf,
}

/// The environment a Seatbelt-confined app gets: the Linux launcher's
/// `HOME`/`XDG_*` contract ([`appdir::sandbox_env`]) pointed at the host
/// container, since there is no mount namespace to hide it behind, plus
/// `TMPDIR`.
pub fn app_environment(container: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = appdir::sandbox_env()
        .into_iter()
        .map(|(key, value)| {
            let rest = value
                .strip_prefix(appdir::SANDBOX_APP_DIR)
                .unwrap_or_default()
                .trim_start_matches('/');
            let host = if rest.is_empty() {
                container.to_path_buf()
            } else {
                container.join(rest)
            };
            (key, host.display().to_string())
        })
        .collect();
    env.push((
        "TMPDIR".to_string(),
        container.join(CONTAINER_TMP).display().to_string(),
    ));
    env
}

/// The network half of a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatbeltNetwork {
    /// No socket may be connected or bound, IP or filesystem.
    Denied,
    /// Only this loopback TCP port — the egress gate — may be connected.
    Gate(u16),
    /// IP traffic in and out, and the system resolver. Filesystem sockets
    /// stay denied: a shared host network is not the user's `ssh-agent`.
    Unrestricted,
}

/// The inputs of one launch, before resolution.
#[derive(Debug, Clone, Copy)]
pub struct PolicyInputs<'a> {
    /// The installed package: readable and executable, never writable.
    pub payload_dir: &'a Path,
    /// Shared runtime dylibs the app links against: readable and mappable.
    pub runtime_dylib_dir: Option<&'a Path>,
    /// The one writable directory (see [`prepare_container`]).
    pub container_dir: &'a Path,
    pub capabilities: &'a CapabilityPlan,
    pub egress: &'a EgressDecision,
    /// The egress gate's loopback port, when [`EgressDecision::Filtered`]
    /// runs one.
    pub gate_port: Option<u16>,
    /// The app's own per-user cache directory (see [`prepare_darwin_cache`]).
    pub darwin_cache: Option<&'a Path>,
}

/// Every grant of one launch, as resolved paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltPolicy {
    pub payload: PathBuf,
    pub runtime: Option<PathBuf>,
    pub container: PathBuf,
    /// Read and write, like the container.
    pub darwin_cache: Option<PathBuf>,
    /// User-granted read-only paths.
    pub read_only: Vec<PathBuf>,
    /// User-granted read-write paths.
    pub read_write: Vec<PathBuf>,
    pub network: SeatbeltNetwork,
    /// Grants the profile could not express. Each is denied, never widened.
    pub not_enforced: Vec<String>,
}

/// A launch input that cannot be confined as asked.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("the {role} directory {path} cannot be resolved: {source}")]
    Resolve {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the {role} directory {path} resolves to the filesystem root")]
    Root { role: &'static str, path: PathBuf },
    #[error(
        "the writable directory {container} and the {role} directory {other} overlap; an app \
         must not be able to write what it executes"
    )]
    Overlap {
        role: &'static str,
        container: PathBuf,
        other: PathBuf,
    },
}

impl SeatbeltPolicy {
    /// Resolve `inputs` into the policy a profile is rendered from.
    ///
    /// A user-granted path that does not exist is left out, as on Linux: it
    /// cannot be granted, and an unresolvable path in a profile would match
    /// nothing anyway.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when the payload, runtime or container
    /// directory cannot be resolved, is the root, or when the container
    /// overlaps a directory the app may execute from.
    pub fn resolve(inputs: &PolicyInputs<'_>) -> Result<Self, PolicyError> {
        let payload = resolve_dir("payload", inputs.payload_dir)?;
        let runtime = inputs
            .runtime_dylib_dir
            .map(|dir| resolve_dir("runtime", dir))
            .transpose()?;
        let container = resolve_dir("container", inputs.container_dir)?;
        let darwin_cache = inputs
            .darwin_cache
            .map(|dir| resolve_dir("cache", dir))
            .transpose()?;
        for writable in std::iter::once(&container).chain(&darwin_cache) {
            for (role, other) in std::iter::once(("payload", &payload))
                .chain(runtime.as_ref().map(|runtime| ("runtime", runtime)))
            {
                if writable.starts_with(other) || other.starts_with(writable) {
                    return Err(PolicyError::Overlap {
                        role,
                        container: writable.clone(),
                        other: other.clone(),
                    });
                }
            }
        }

        let mut not_enforced = Vec::new();
        let read_write = resolve_grants(&inputs.capabilities.rw_paths, &mut not_enforced);
        let read_only: Vec<PathBuf> =
            resolve_grants(&inputs.capabilities.ro_paths, &mut not_enforced)
                .into_iter()
                .filter(|path| !read_write.contains(path))
                .collect();
        for device in &inputs.capabilities.devices {
            not_enforced.push(format!(
                "device `{device}`: macOS gates capture devices through TCC, which the \
                 Seatbelt profile does not grant"
            ));
        }

        let network = match inputs.egress {
            EgressDecision::Blocked => SeatbeltNetwork::Denied,
            EgressDecision::Unrestricted => SeatbeltNetwork::Unrestricted,
            EgressDecision::Filtered(_) => {
                if let Some(port) = inputs.gate_port {
                    SeatbeltNetwork::Gate(port)
                } else {
                    not_enforced.push(
                        "filtered egress: no gate is running; the app is offline".to_string(),
                    );
                    SeatbeltNetwork::Denied
                }
            }
        };

        Ok(SeatbeltPolicy {
            payload,
            runtime,
            container,
            darwin_cache,
            read_only,
            read_write,
            network,
            not_enforced,
        })
    }
}

fn resolve_dir(role: &'static str, path: &Path) -> Result<PathBuf, PolicyError> {
    let resolved = std::fs::canonicalize(path).map_err(|source| PolicyError::Resolve {
        role,
        path: path.to_path_buf(),
        source,
    })?;
    if is_root(&resolved) {
        return Err(PolicyError::Root {
            role,
            path: path.to_path_buf(),
        });
    }
    Ok(resolved)
}

fn resolve_grants(paths: &[PathBuf], not_enforced: &mut Vec<String>) -> Vec<PathBuf> {
    let mut resolved: Vec<PathBuf> = Vec::new();
    for path in paths {
        match std::fs::canonicalize(path) {
            // A symlink can turn an innocuous grant into the whole disk.
            Ok(real) if is_root(&real) => not_enforced.push(format!(
                "{} resolves to the filesystem root; not granted",
                path.display()
            )),
            Ok(real) => {
                if !resolved.contains(&real) {
                    resolved.push(real);
                }
            }
            Err(e) => tracing::info!(
                target: "tilo_launcher::macos",
                path = %path.display(),
                "granted path does not resolve, leaving it out: {e}"
            ),
        }
    }
    resolved
}

fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher::netgate::EgressPolicy;

    struct Layout {
        _base: tempfile::TempDir,
        root: PathBuf,
        payload: PathBuf,
        container: PathBuf,
    }

    fn layout() -> Layout {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().to_path_buf();
        let payload = root.join("payload");
        let container = root.join("container");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::create_dir_all(&container).unwrap();
        Layout {
            _base: base,
            root,
            payload,
            container,
        }
    }

    fn inputs<'a>(
        layout: &'a Layout,
        plan: &'a CapabilityPlan,
        egress: &'a EgressDecision,
    ) -> PolicyInputs<'a> {
        PolicyInputs {
            payload_dir: &layout.payload,
            runtime_dylib_dir: None,
            container_dir: &layout.container,
            capabilities: plan,
            egress,
            gate_port: None,
            darwin_cache: None,
        }
    }

    #[test]
    fn paths_are_resolved_because_seatbelt_matches_real_paths() {
        let l = layout();
        let link = l.root.join("payload-link");
        std::os::unix::fs::symlink(&l.payload, &link).unwrap();
        let plan = CapabilityPlan::empty();
        let egress = EgressDecision::Blocked;
        let mut i = inputs(&l, &plan, &egress);
        i.payload_dir = &link;
        let policy = SeatbeltPolicy::resolve(&i).unwrap();
        assert_eq!(policy.payload, std::fs::canonicalize(&l.payload).unwrap());
        assert!(policy.payload.is_absolute());
        assert_eq!(policy.network, SeatbeltNetwork::Denied);
        assert!(policy.not_enforced.is_empty());
    }

    #[test]
    fn a_container_overlapping_the_payload_is_refused() {
        let l = layout();
        let inner = l.payload.join("data");
        std::fs::create_dir_all(&inner).unwrap();
        let plan = CapabilityPlan::empty();
        let egress = EgressDecision::Blocked;
        let mut i = inputs(&l, &plan, &egress);
        i.container_dir = &inner;
        assert!(matches!(
            SeatbeltPolicy::resolve(&i),
            Err(PolicyError::Overlap {
                role: "payload",
                ..
            })
        ));
        i.container_dir = &l.root;
        assert!(matches!(
            SeatbeltPolicy::resolve(&i),
            Err(PolicyError::Overlap { .. })
        ));
    }

    #[test]
    fn missing_or_root_inputs_are_refused_or_dropped() {
        let l = layout();
        let plan = CapabilityPlan::empty();
        let egress = EgressDecision::Blocked;
        let missing = l.root.join("nope");
        let mut i = inputs(&l, &plan, &egress);
        i.payload_dir = &missing;
        assert!(matches!(
            SeatbeltPolicy::resolve(&i),
            Err(PolicyError::Resolve {
                role: "payload",
                ..
            })
        ));
        i.payload_dir = Path::new("/");
        assert!(matches!(
            SeatbeltPolicy::resolve(&i),
            Err(PolicyError::Root { .. })
        ));

        let to_root = l.root.join("to-root");
        std::os::unix::fs::symlink("/", &to_root).unwrap();
        let plan = CapabilityPlan {
            ro_paths: vec![to_root, l.root.join("missing")],
            ..CapabilityPlan::empty()
        };
        let policy = SeatbeltPolicy::resolve(&inputs(&l, &plan, &egress)).unwrap();
        assert!(policy.read_only.is_empty(), "{policy:?}");
        assert_eq!(policy.not_enforced.len(), 1, "{policy:?}");
    }

    #[test]
    fn user_grants_map_onto_read_only_and_read_write() {
        let l = layout();
        let docs = l.root.join("docs");
        let music = l.root.join("music");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::create_dir_all(&music).unwrap();
        let plan = CapabilityPlan {
            ro_paths: vec![music.clone(), docs.clone()],
            rw_paths: vec![docs.clone()],
            devices: vec!["camera".to_string()],
            ..CapabilityPlan::empty()
        };
        let egress = EgressDecision::Blocked;
        let policy = SeatbeltPolicy::resolve(&inputs(&l, &plan, &egress)).unwrap();
        let real = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(policy.read_write, vec![real(&docs)]);
        assert_eq!(policy.read_only, vec![real(&music)]);
        assert_eq!(policy.not_enforced.len(), 1);
        assert!(policy.not_enforced[0].contains("camera"));
    }

    #[test]
    fn egress_maps_without_ever_widening() {
        let l = layout();
        let plan = CapabilityPlan::empty();
        let unrestricted = EgressDecision::Unrestricted;
        let policy = SeatbeltPolicy::resolve(&inputs(&l, &plan, &unrestricted)).unwrap();
        assert_eq!(policy.network, SeatbeltNetwork::Unrestricted);

        let filtered = EgressDecision::Filtered(EgressPolicy::from_grants(["example.com"]));
        let offline = SeatbeltPolicy::resolve(&inputs(&l, &plan, &filtered)).unwrap();
        assert_eq!(offline.network, SeatbeltNetwork::Denied);
        assert_eq!(offline.not_enforced.len(), 1);

        let mut i = inputs(&l, &plan, &filtered);
        i.gate_port = Some(9050);
        let gated = SeatbeltPolicy::resolve(&i).unwrap();
        assert_eq!(gated.network, SeatbeltNetwork::Gate(9050));
        assert!(gated.not_enforced.is_empty());
    }

    #[test]
    fn the_environment_is_the_linux_contract_pointed_at_the_container() {
        let container = Path::new("/Users/u/Library/Application Support/Tilo/tilo-rooms/r/p");
        let env = app_environment(container);
        let linux: Vec<String> = appdir::sandbox_env().into_iter().map(|(k, _)| k).collect();
        for key in &linux {
            assert!(env.iter().any(|(k, _)| k == key), "{key} missing");
        }
        for (key, value) in &env {
            assert!(
                Path::new(value).starts_with(container),
                "{key}={value} leaves the container"
            );
        }
        let get = |k: &str| env.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("HOME"), container.display().to_string());
        assert_eq!(
            get("XDG_DATA_HOME"),
            container.join(".local/share").display().to_string()
        );
        assert_eq!(get("TMPDIR"), container.join("tmp").display().to_string());
    }

    #[test]
    fn each_container_gets_its_own_cache_directory() {
        let data = Path::new("/Users/u/Library/Application Support/Tilo");
        let suffix = |room: &str, package: &str| {
            darwin_dir_suffix(&container_dir(data, Some(room), package)).unwrap()
        };
        assert_eq!(suffix("r", "so.tilo.notes"), "so.tilo+r+so.tilo.notes");
        assert_ne!(suffix("a.b", "c"), suffix("a", "b.c"));
        assert_ne!(suffix("a", "so.tilo.notes"), suffix("b", "so.tilo.notes"));
        assert!(!suffix("../x", "a/b").contains('/'));

        let tmp = tempfile::tempdir().unwrap();
        let user_cache = tmp.path().join("C");
        std::fs::create_dir_all(&user_cache).unwrap();
        let container = container_dir(tmp.path(), Some("r"), "so.tilo.notes");
        let cache = prepare_darwin_cache(&user_cache, &container).unwrap();
        assert_eq!(cache.suffix, "so.tilo+r+so.tilo.notes");
        assert_eq!(
            cache.dir,
            std::fs::canonicalize(&user_cache)
                .unwrap()
                .join(&cache.suffix)
        );
        assert_eq!(
            prepare_darwin_cache(&user_cache, &container).unwrap(),
            cache
        );

        let other = container_dir(tmp.path(), Some("r"), "so.tilo.other");
        std::os::unix::fs::symlink(
            &cache.dir,
            user_cache.join(darwin_dir_suffix(&other).unwrap()),
        )
        .unwrap();
        assert!(
            prepare_darwin_cache(&user_cache, &other).is_err(),
            "a link to another app's cache was accepted"
        );
    }

    #[test]
    fn the_cache_directory_is_writable_and_never_executable() {
        let l = layout();
        let plan = CapabilityPlan::empty();
        let egress = EgressDecision::Blocked;
        let cache = l.root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut i = inputs(&l, &plan, &egress);
        i.darwin_cache = Some(&cache);
        let policy = SeatbeltPolicy::resolve(&i).unwrap();
        assert_eq!(
            policy.darwin_cache,
            Some(std::fs::canonicalize(&cache).unwrap())
        );

        let inside = l.payload.join("cache");
        std::fs::create_dir_all(&inside).unwrap();
        i.darwin_cache = Some(&inside);
        assert!(matches!(
            SeatbeltPolicy::resolve(&i),
            Err(PolicyError::Overlap {
                role: "payload",
                ..
            })
        ));
    }

    #[test]
    fn containers_live_beside_the_runtime_profile_per_room_and_package() {
        let home = Path::new("/Users/u");
        let data = data_home(home);
        assert_eq!(data, Path::new("/Users/u/Library/Application Support/Tilo"));
        assert_eq!(
            container_dir(&data, None, "so.tilo.notes"),
            data.join("tilo-rooms/personal/so.tilo.notes")
        );
        assert_ne!(
            container_dir(&data, Some("a"), "so.tilo.notes"),
            container_dir(&data, Some("b"), "so.tilo.notes")
        );
        assert!(
            container_dir(&data, Some("../../x"), "../..").starts_with(data.join("tilo-rooms"))
        );

        let tmp = tempfile::tempdir().unwrap();
        let dir = prepare_container(tmp.path(), None, "so.tilo.notes").unwrap();
        assert!(dir.join(CONTAINER_TMP).is_dir());
        assert!(dir.join(".config").is_dir());
    }
}
