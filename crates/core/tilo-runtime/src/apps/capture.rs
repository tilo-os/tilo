//! Where a capture writes: the directories a `--headless-drive` spec names for its pictures and
//! reports, granted to the app's sandbox for that one launch.
//!
//! A capture runs the app in its own sandbox, which writes nowhere outside the app's own data. The
//! pictures and reports it names land in a directory of the caller's, so the launch binds those
//! directories writable, and nothing else: only for the person on this device's own connection
//! (the admission that installs a development build), only for a development build this device
//! made, and only directories that person owns and that hold none of the device's own state. The
//! grant is the launch's alone and is never written into the workspace.

use std::path::{Path, PathBuf};

/// The directories a capture writes into, granted for its launch alone, and where the launch's
/// capability file is written apart from every other launch's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Writes {
    /// Each directory, absolute and canonical, bound writable into the sandbox.
    pub dirs: Vec<PathBuf>,
    /// The directory holding this launch's own capability file, removed when the capture ends.
    pub grants_dir: PathBuf,
}

impl Writes {
    /// The grants these writes add to the launch of `app`: each directory writable, through the
    /// app alone, for this one launch.
    pub(crate) fn grants(&self, app: &str) -> Vec<tilo_permissions::caps::CapOverride> {
        use tilo_action_policy::Grantee;
        use tilo_action_types::Lifetime;
        use tilo_permissions::caps::Access;
        use tilo_permissions::caps::CapOverride;
        use tilo_permissions::caps::Scope;
        self.dirs
            .iter()
            .map(|dir| {
                let path = dir.to_string_lossy().into_owned();
                let mut grant = CapOverride::grant(
                    Grantee::App(app.to_owned()),
                    Scope::FsPath(path.clone()),
                    Access::Write,
                )
                .through(app);
                grant.lifetime = Lifetime::Once;
                grant.id = format!("capture:{path}");
                grant
            })
            .collect()
    }
}

/// The instance a capture's process joins its workspace as, the person at it, and the
/// collaboration, identity and content connections this daemon made for it, attested as that
/// instance: the capture's calls go through the device's dispatch as a launched window's do.
pub(crate) struct Joined {
    pub instance: u64,
    pub person: String,
    pub collaboration: runtime_portal::process_link::Descriptor,
    pub identity: runtime_portal::process_link::Descriptor,
    pub content: runtime_portal::process_link::Descriptor,
}

/// Whether the capture `args` of `writer`'s app joins the workspace its device serves, as a spec
/// asks (`"workspace": true`): its instance runs over the workspace's one store, a scenario it
/// stages is written there and what it calls goes through the device's dispatch. Only the person
/// on this device's own connection has a development build this device made join; a capture that
/// does not ask runs over a store of its own and reaches nothing of the workspace.
///
/// # Errors
/// A spec that does not parse, or a capture asking to join for a caller other than the person on
/// this device's own connection, or of an app that is not this device's development build.
pub(crate) fn joins(
    args: &[String],
    caller: impl FnOnce() -> Result<tilo_actions::remote::Caller, String>,
    writer: &Writer<'_>,
) -> Result<bool, String> {
    if !specs(args)?.iter().any(|spec| spec.workspace) {
        return Ok(false);
    }
    admit(&caller()?, writer).map_err(|_| {
        format!(
            "a capture joins its workspace only when the person on this device's own connection \
             starts it, for a development build this device made: {} is refused",
            writer.app_id
        )
    })?;
    Ok(true)
}

/// Who may have a capture write outside its sandbox, and for which app.
pub(crate) struct Writer<'a> {
    /// Whether this daemon is its account's own, as the device's package cache reads it.
    pub personal: bool,
    /// The app the capture runs, as installed.
    pub app_id: &'a str,
    /// `[package.dev]` of the installed app, when it is a development build.
    pub dev: Option<&'a tilo_app_package::manifest::DevBuild>,
    /// This device's public key.
    pub device: [u8; 32],
    /// The account of the process that asked, as the kernel names it.
    pub uid: Option<u32>,
    /// What no grant may hold: the home, the workspace's files and the device's own state.
    pub protected: Vec<PathBuf>,
}

/// The directories `args`, a capture's own arguments, write into, checked and admitted for
/// `caller`: empty when the capture names nothing to write.
///
/// # Errors
/// A spec that does not parse, a path that is not absolute, a caller other than the person on this
/// device's own connection, an app that is not this device's development build, or a directory
/// that is not one the person owns apart from the device's own state.
pub(crate) fn writes(
    args: &[String],
    caller: impl FnOnce() -> Result<tilo_actions::remote::Caller, String>,
    writer: &Writer<'_>,
) -> Result<Vec<PathBuf>, String> {
    let named = named_dirs(args)?;
    if named.is_empty() {
        return Ok(Vec::new());
    }
    admit(&caller()?, writer)?;
    let uid = writer
        .uid
        .ok_or("the kernel does not say whose process asks for the capture")?;
    let protected: Vec<PathBuf> = writer
        .protected
        .iter()
        .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
        .collect();
    let mut dirs = Vec::new();
    for dir in named {
        let dir = writable(&dir, uid, &protected)?;
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    Ok(dirs)
}

/// Whether `caller` may have `writer`'s app write outside its sandbox: the person on this device's
/// own connection, for a development build this device made.
fn admit(caller: &tilo_actions::remote::Caller, writer: &Writer<'_>) -> Result<(), String> {
    package_actions::dev_build_admitted(caller, writer.personal).map_err(|_| {
        "a capture writes outside its app's sandbox only when the person on this device's own \
         connection starts it, never an agent, another member or the app itself"
            .to_owned()
    })?;
    let built_here = writer.dev.is_some_and(|dev| {
        dev.device_bytes().is_ok_and(|key| key == writer.device)
            && writer.app_id == tilo_app_package::dev_app_id(&dev.of, &writer.device)
    });
    if !built_here {
        return Err(format!(
            "{} is not a development build this device made: a capture of it writes nowhere \
             outside its sandbox",
            writer.app_id
        ));
    }
    Ok(())
}

/// The `--headless-drive` specs in `args`, parsed as the app parses them.
fn specs(args: &[String]) -> Result<Vec<tilo_headless_schema::Spec>, String> {
    let mut raw = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--headless-drive" {
            raw.extend(rest.next().map(String::as_str));
        } else if let Some(spec) = arg.strip_prefix("--headless-drive=") {
            raw.push(spec);
        }
    }
    raw.into_iter()
        .map(|spec| {
            tilo_headless_schema::parse_spec(spec)
                .map_err(|error| format!("the capture's --headless-drive: {error}"))
        })
        .collect()
}

/// The directories holding every file the `--headless-drive` specs in `args` write: its pictures,
/// its results and its reports.
fn named_dirs(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for spec in specs(args)? {
        let files = spec
            .captures
            .iter()
            .map(|capture| &capture.path)
            .chain(spec.results.iter())
            .chain(spec.resources.iter())
            .chain(spec.audit_report.iter())
            .chain(spec.dump.iter())
            .chain(spec.targets.iter())
            .chain(spec.inspect.iter());
        for file in files {
            let file = Path::new(file);
            if !file.is_absolute() {
                return Err(format!(
                    "a capture names where it writes in full: {} is a relative path",
                    file.display()
                ));
            }
            let dir = file
                .parent()
                .filter(|dir| dir.file_name().is_some())
                .ok_or_else(|| format!("{} is no file in a directory", file.display()))?;
            if !dirs.iter().any(|named| named == dir) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    Ok(dirs)
}

/// `dir` as a capture may write into it: an existing directory of `uid`'s, named by its own
/// canonical path, holding none of `protected`.
fn writable(dir: &Path, uid: u32, protected: &[PathBuf]) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt as _;
    let shown = dir.display();
    let metadata = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("a capture writes into {shown}, which it cannot read: {error}"))?;
    if !metadata.is_dir() {
        return Err(format!(
            "a capture writes into {shown}, which is not a directory"
        ));
    }
    let real = std::fs::canonicalize(dir)
        .map_err(|error| format!("a capture writes into {shown}: {error}"))?;
    if real != dir {
        return Err(format!(
            "a capture writes into {shown}, a link to {}: name the directory itself",
            real.display()
        ));
    }
    if metadata.uid() != uid {
        return Err(format!(
            "a capture writes into {shown}, which is not a directory of the person asking"
        ));
    }
    if real.to_str().is_none() {
        return Err(format!(
            "a capture writes into {shown}, a path that is not text"
        ));
    }
    if real.parent().is_none() || protected.iter().any(|held| held.starts_with(&real)) {
        return Err(format!(
            "a capture writes into {shown}, which holds the home, the workspace's files or the \
             device's own state"
        ));
    }
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt as _;
    use tilo_actions::remote::Caller;

    const APP: &str = "so.tilo.tube";
    const DEVICE: [u8; 32] = [7; 32];

    fn dev() -> tilo_app_package::manifest::DevBuild {
        tilo_app_package::manifest::DevBuild {
            device: format!("ed25519:{}", hex::encode(DEVICE)),
            of: APP.to_owned(),
        }
    }

    fn drive(dir: &Path) -> Vec<String> {
        let spec = serde_json::json!({
            "capture": [{"path": dir.join("wide.png")}, {"path": dir.join("compact.png")}],
            "results": dir.join("results.json"),
        });
        vec!["--headless-drive".to_owned(), spec.to_string()]
    }

    fn person() -> Caller {
        Caller::own_person("person-7")
    }

    // A capture's pictures land in a directory of the person who started it, granted to that
    // launch alone: only for the person on this device's own connection, and only for the
    // development build this device made.
    #[test]
    fn a_capture_names_where_it_writes_and_only_its_person_grants_it() {
        let temp = tempfile::tempdir().unwrap();
        let out = std::fs::canonicalize(temp.path()).unwrap().join("out");
        std::fs::create_dir(&out).unwrap();
        let uid = std::fs::metadata(&out).unwrap().uid();
        let dev_id = tilo_app_package::dev_app_id(APP, &DEVICE);
        let dev = dev();
        let writer = Writer {
            personal: true,
            app_id: &dev_id,
            dev: Some(&dev),
            device: DEVICE,
            uid: Some(uid),
            protected: vec![temp.path().join("home")],
        };

        // The person's own capture of the device's development build: exactly its directory.
        assert_eq!(
            writes(&drive(&out), || Ok(person()), &writer).unwrap(),
            vec![out.clone()]
        );
        // A capture that writes nothing asks for nothing, whoever asks.
        assert!(
            writes(
                &["--headless-drive".to_owned(), "{}".to_owned()],
                || Ok(Caller::launched(dev_id.clone())),
                &writer
            )
            .unwrap()
            .is_empty()
        );

        // The released app's capture gets no path.
        let released = Writer {
            app_id: APP,
            dev: None,
            ..writer
        };
        assert!(writes(&drive(&out), || Ok(person()), &released).is_err());
        // Nor does another device's development build, under its own id.
        let other = tilo_app_package::manifest::DevBuild {
            device: format!("ed25519:{}", hex::encode([9_u8; 32])),
            of: APP.to_owned(),
        };
        let theirs = Writer {
            dev: Some(&other),
            ..released
        };
        assert!(writes(&drive(&out), || Ok(person()), &theirs).is_err());
    }

    // A capture naming a path that comes from an agent, another member or the app itself is
    // refused before any directory is read: the grant is the device person's alone.
    #[test]
    fn a_capture_path_from_an_agent_a_member_or_the_app_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let out = std::fs::canonicalize(temp.path()).unwrap();
        let uid = std::fs::metadata(&out).unwrap().uid();
        let dev_id = tilo_app_package::dev_app_id(APP, &DEVICE);
        let dev = dev();
        let writer = Writer {
            personal: true,
            app_id: &dev_id,
            dev: Some(&dev),
            device: DEVICE,
            uid: Some(uid),
            protected: Vec::new(),
        };
        let askers = [
            ("an agent", Caller::local_agent("agent-1", "person-7")),
            ("another member", Caller::authenticated([3; 32], [4; 32])),
            ("the app itself", Caller::launched(dev_id.clone())),
        ];
        for (who, caller) in askers {
            let refused = writes(&drive(&out), || Ok(caller), &writer)
                .expect_err(&format!("{who}'s capture path was granted"));
            assert!(
                refused.contains("own connection"),
                "{who} is told who may grant it: {refused}"
            );
        }
    }

    // A capture joins the workspace only when it asks, and only as the person's own capture of
    // the device's development build: one an agent, another member or the app asks for, and one
    // of a release, is refused.
    #[test]
    fn a_capture_joins_its_workspace_only_as_its_persons_capture_of_the_devices_build() {
        let dev_id = tilo_app_package::dev_app_id(APP, &DEVICE);
        let dev = dev();
        let writer = Writer {
            personal: true,
            app_id: &dev_id,
            dev: Some(&dev),
            device: DEVICE,
            uid: None,
            protected: Vec::new(),
        };
        let asks = [
            "--headless-drive".to_owned(),
            r#"{"workspace":true}"#.to_owned(),
        ];
        let keeps_its_own = ["--headless-drive".to_owned(), "{}".to_owned()];
        assert_eq!(joins(&asks, || Ok(person()), &writer), Ok(true));
        assert_eq!(joins(&keeps_its_own, || Ok(person()), &writer), Ok(false));
        for (who, caller) in [
            ("an agent", Caller::local_agent("agent-1", "person-7")),
            ("another member", Caller::authenticated([3; 32], [4; 32])),
            ("the app itself", Caller::launched(dev_id.clone())),
        ] {
            let refused = joins(&asks, || Ok(caller.clone()), &writer)
                .expect_err(&format!("{who}'s capture joined the workspace"));
            assert!(
                refused.contains("own connection"),
                "{who} is told why: {refused}"
            );
            // Asking for nothing, it is no one's business who started it.
            assert_eq!(joins(&keeps_its_own, || Ok(caller), &writer), Ok(false));
        }
        let released = Writer {
            app_id: APP,
            dev: None,
            ..writer
        };
        assert!(
            joins(&asks, || Ok(person()), &released).is_err(),
            "a release's capture joined the workspace"
        );
    }

    // Each directory is checked as the device reads it: an existing directory of the person's,
    // named by its own path, holding none of the home, the workspace's files or the device's state.
    #[test]
    fn a_capture_writes_only_into_a_plain_directory_of_its_person() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let out = root.join("out");
        std::fs::create_dir_all(home.join("files")).unwrap();
        std::fs::create_dir(&out).unwrap();
        std::os::unix::fs::symlink(&out, root.join("link")).unwrap();
        std::fs::write(root.join("file"), b"").unwrap();
        let uid = std::fs::metadata(&out).unwrap().uid();
        let protected = [home.join("files")];

        assert_eq!(writable(&out, uid, &protected).unwrap(), out);
        assert!(writable(&root.join("missing"), uid, &protected).is_err());
        assert!(writable(&root.join("file"), uid, &protected).is_err());
        assert!(
            writable(&root.join("link"), uid, &protected).is_err(),
            "a link"
        );
        assert!(
            writable(&out.join("..").join("out"), uid, &protected).is_err(),
            "a path that is not its own"
        );
        assert!(
            writable(&out, uid.wrapping_add(1), &protected).is_err(),
            "another account's"
        );
        assert!(
            writable(&home, uid, &protected).is_err(),
            "a parent of the files"
        );
        assert!(
            writable(&home.join("files"), uid, &protected).is_err(),
            "the files"
        );
        assert!(writable(Path::new("/"), uid, &[]).is_err(), "the root");

        // A spec names its files in full.
        let relative = vec![
            "--headless-drive".to_owned(),
            r#"{"capture":{"path":"wide.png"}}"#.to_owned(),
        ];
        assert!(named_dirs(&relative).is_err());
        // Every file it writes counts, inline or not.
        let inline = vec![format!(
            "--headless-drive={}",
            serde_json::json!({"resources": out.join("resources.json"), "inspect": root.join("tree.json")})
        )];
        assert_eq!(named_dirs(&inline).unwrap(), vec![out, root]);
    }
}
