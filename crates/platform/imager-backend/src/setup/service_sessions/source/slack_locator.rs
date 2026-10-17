//! Fixed native path and canonical Flatpak deployment; no executable launches.
use super::{SourceError, slack_linux::SlackApplication};
use crate::setup::flatpak_transfer::{NativeRunner, Runner, discover};
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
    sync::Arc,
};
struct Edge {
    parent: File,
    name: std::ffi::CString,
    identity: (u64, u64, u32, u32),
}
pub(super) struct PublicFile {
    pub file: File,
    edges: Vec<Edge>,
    facts: std::fs::Metadata,
}
fn denied() -> SourceError {
    SourceError::UnsupportedEncryption
}
fn safe(facts: &std::fs::Metadata) -> bool {
    let owner = rustix::process::getuid().as_raw();
    (facts.uid() == 0 || facts.uid() == owner)
        && (facts.mode() & 0o022 == 0
            || facts.is_dir() && facts.uid() == 0 && facts.mode() & 0o1000 != 0)
}
impl PublicFile {
    pub(super) fn open(path: &Path) -> Result<Self, SourceError> {
        if !path.is_absolute() {
            return Err(denied());
        }
        let mut current = File::open("/").map_err(|_| denied())?;
        let mut edges = Vec::new();
        let names: Vec<_> = path
            .components()
            .filter_map(|part| match part {
                Component::RootDir => None,
                Component::Normal(name) => Some(Ok(name)),
                _ => Some(Err(denied())),
            })
            .collect::<Result<_, _>>()?;
        if names.is_empty() || names.len() > 64 {
            return Err(denied());
        }
        for (index, name) in names.iter().enumerate() {
            if !safe(&current.metadata().map_err(|_| denied())?) {
                return Err(denied());
            }
            let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| denied())?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if index + 1 < names.len() {
                    libc::O_DIRECTORY
                } else {
                    0
                };
            let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(denied());
            }
            let next = unsafe { File::from_raw_fd(fd) };
            let facts = next.metadata().map_err(|_| denied())?;
            if !safe(&facts) || index + 1 == names.len() && !facts.is_file() {
                return Err(denied());
            }
            edges.push(Edge {
                parent: current,
                name,
                identity: (facts.dev(), facts.ino(), facts.uid(), facts.mode()),
            });
            current = next;
        }
        let facts = current.metadata().map_err(|_| denied())?;
        let this = Self {
            file: current,
            edges,
            facts,
        };
        this.validate()?;
        Ok(this)
    }
    pub(super) fn validate(&self) -> Result<(), SourceError> {
        for edge in &self.edges {
            if !safe(&edge.parent.metadata().map_err(|_| denied())?) {
                return Err(denied());
            }
            let mut actual: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::fstatat(
                    edge.parent.as_raw_fd(),
                    edge.name.as_ptr(),
                    &raw mut actual,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
                || (actual.st_dev, actual.st_ino, actual.st_uid, actual.st_mode) != edge.identity
            {
                return Err(denied());
            }
        }
        let after = self.file.metadata().map_err(|_| denied())?;
        let before = &self.facts;
        if (
            after.dev(),
            after.ino(),
            after.uid(),
            after.mode(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) != (
            before.dev(),
            before.ino(),
            before.uid(),
            before.mode(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) {
            return Err(denied());
        }
        Ok(())
    }
}
/// Called only by migration-consented source discovery, never by presence reading.
/// The public package check grants no profile or keychain access.
pub fn native() -> Option<Arc<SlackApplication>> {
    ["/usr/lib/slack"]
        .into_iter()
        .find_map(|root| SlackApplication::at(Path::new(root)).ok())
}
pub fn flatpak(home: &Path) -> Option<Arc<SlackApplication>> {
    flatpak_with(&NativeRunner, home).ok()
}
fn flatpak_with(runner: &impl Runner, home: &Path) -> Result<Arc<SlackApplication>, SourceError> {
    let inventory = discover(runner, home).map_err(|_| denied())?;
    let selected: Vec<_> = inventory
        .refs
        .iter()
        .filter(|item| item.app_id() == Some("com.slack.Slack"))
        .collect();
    // Ambiguous installations cannot bind one profile to a unique application.
    if selected.len() != 1 {
        return Err(denied());
    }
    let item = selected[0];
    let args = [
        "info".into(),
        item.installation.argument(),
        "--show-location".into(),
        item.reference.clone(),
    ];
    let location = runner.run(home, &args).map_err(|_| denied())?;
    let path = Path::new(location.trim_end_matches('\n'));
    let parts: Vec<_> = path.components().collect();
    let reference: Vec<_> = item.reference.split('/').collect();
    if !path.is_absolute() || parts.len() < 5 {
        return Err(denied());
    }
    let suffix = &parts[parts.len() - 5..];
    for (part, expected) in suffix[..4].iter().zip(reference) {
        if part.as_os_str() != expected {
            return Err(denied());
        }
    }
    let commit = suffix[4].as_os_str().to_str().ok_or_else(denied)?;
    if commit.len() != 64
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(denied());
    }
    let proof = SlackApplication::at(&path.join("files/extra"))?;
    if runner.run(home, &args).map_err(|_| denied())? != location {
        return Err(denied());
    }
    proof.validate()?;
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn public_package_descriptor_refuses_links_replacement_and_foreign_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("public");
        std::fs::write(&path, b"public pinned bytes").unwrap();
        let held = PublicFile::open(&path).unwrap();
        held.validate().unwrap();
        std::fs::rename(&path, directory.path().join("old")).unwrap();
        std::fs::write(&path, b"public pinned bytes").unwrap();
        assert!(held.validate().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(PublicFile::open(&path).is_err());
        let link = directory.path().join("link");
        symlink(directory.path().join("old"), &link).unwrap();
        assert!(PublicFile::open(&link).is_err());
        let fifo = directory.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(PublicFile::open(&fifo).is_err());
    }
}

#[cfg(all(test, feature = "native-slack-qualification"))]
mod native_locator {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const LOCATION: &str = "/fixture/flatpak/app/com.slack.Slack/x86_64/stable/1111111111111111111111111111111111111111111111111111111111111111";
    #[derive(Clone, Copy)]
    enum Case {
        Positive,
        Duplicate,
        RefMismatch,
        MalformedCommit,
        ChangedLocation,
        MissingPackage,
    }
    struct Metadata {
        case: Case,
        locations: AtomicUsize,
    }
    impl Runner for Metadata {
        fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
            let words: Vec<_> = args.iter().map(String::as_str).collect();
            if words
                == [
                    "list",
                    "--all",
                    "--app",
                    "--columns=ref:f,origin:f,installation:f,name:f",
                ]
            {
                let mut rows = "com.slack.Slack/x86_64/stable\tflathub\tuser\tSlack\n".to_owned();
                if matches!(self.case, Case::Duplicate) {
                    rows.push_str("com.slack.Slack/x86_64/stable\tflathub\tsystem\tSlack\n");
                }
                return Ok(rows);
            }
            if words
                == [
                    "list",
                    "--all",
                    "--runtime",
                    "--columns=ref:f,origin:f,installation:f,name:f",
                ]
            {
                return Ok(String::new());
            }
            if words.first() == Some(&"remotes")
                && words.len() == 4
                && words[2..] == ["--show-disabled", "--columns=name:f,url:f,options:f"]
            {
                return Ok("flathub\thttps://example.test/flatpak\t\n".into());
            }
            assert_eq!(
                words,
                [
                    "info",
                    "--user",
                    "--show-location",
                    "app/com.slack.Slack/x86_64/stable"
                ]
            );
            let call = self.locations.fetch_add(1, Ordering::SeqCst);
            Ok(match self.case {
                Case::RefMismatch => LOCATION.replace("com.slack.Slack", "com.example.Other"),
                Case::MalformedCommit => LOCATION.replace(&"1".repeat(64), "short"),
                Case::ChangedLocation if call > 0 => {
                    LOCATION.replace(&"1".repeat(64), &"2".repeat(64))
                }
                Case::MissingPackage => {
                    LOCATION.replace("/fixture/flatpak", "/fixture/missing-flatpak")
                }
                _ => LOCATION.to_owned(),
            } + "\n")
        }
    }
    #[test]
    fn native_pinned_slack_locator_accepts_genuine_package_and_refuses_deployment_changes() {
        let marker = PublicFile::open(Path::new("/run/tilo-native-chromium-qualification"))
            .expect("explicit isolated native fixture required");
        assert_eq!(marker.file.metadata().unwrap().uid(), 0);
        assert_ne!(rustix::process::getuid().as_raw(), 0);
        let native = native().expect("exact pinned public package mounted at fixed native root");
        native.validate().unwrap();
        // Native snap retains stale ASAR module metadata; documented Flatpak
        // repack regenerates it from the actual distributed unpacked modules.
        let original = std::fs::read("/fixture/slack/resources/app.asar").unwrap();
        let repacked = std::fs::read(Path::new(LOCATION).join("files/extra/resources/app.asar"))
            .expect("documented desktopName + actual-module ASAR repack fixture required");
        assert_ne!(original, repacked);
        let original_modules = super::super::slack_asar::verify(&original).unwrap();
        let repacked_modules = super::super::slack_asar::verify(&repacked).unwrap();
        assert_ne!(original_modules, repacked_modules);
        assert!(
            repacked
                .windows(b"com.slack.Slack.desktop".len())
                .any(|part| part == b"com.slack.Slack.desktop")
        );
        let mut mutated = repacked.clone();
        *mutated.last_mut().unwrap() ^= 1;
        assert!(super::super::slack_asar::verify(&mutated).is_err());
        let home = tempfile::tempdir().unwrap();
        for case in [
            Case::Positive,
            Case::Duplicate,
            Case::RefMismatch,
            Case::MalformedCommit,
            Case::ChangedLocation,
            Case::MissingPackage,
        ] {
            let runner = Metadata {
                case,
                locations: AtomicUsize::new(0),
            };
            let result = flatpak_with(&runner, home.path());
            if matches!(case, Case::Positive) {
                result.unwrap().validate().unwrap();
                assert_eq!(runner.locations.load(Ordering::SeqCst), 2);
            } else {
                assert!(result.is_err());
            }
            native.validate().unwrap();
            marker.validate().unwrap();
        }
    }
}
