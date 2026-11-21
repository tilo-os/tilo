//! Where a server keeps the keys of the sealed stores in its state.
//!
//! What a workspace keeps on its server to sign in with (its sign-ins,
//! provider keys and vault items) is one sealed file in the server's state,
//! and the server opens it with a key it keeps as a file, under the
//! workspace's folder within the folder named here (`workspaces/`). Its
//! operator names a folder apart from the state for those keys, so that a
//! copy or a backup of the state alone opens nobody's store. The setting and
//! the lines logged still say people's stores: what the deployments and
//! scripts that read them name.
//!
//! This decides, once as the server opens, whether the folder named keeps
//! them, and says in the log where they are kept. Only a server's operator is
//! asked: a person's own device names no folder and says nothing.
//!
//! A folder is used only when it is there already. The server makes none: a
//! folder nobody made (a path with a slip in it, a volume that was not
//! mounted) is one the next start may not find, and a key written there is
//! lost with it. A server that keeps the keys in its state says so, as a
//! warning, every time it starts.

use std::path::{Path, PathBuf};

/// The setting a server's operator names the folder with.
const SETTING: &str = "TILO_PEOPLE_KEYS";

/// What the lines are logged as.
const LOG: &str = "tilo_server::people_keys";

/// Where the keys of people's sealed stores are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kept {
    /// Under this folder, apart from the state.
    Apart(PathBuf),
    /// Under this folder, which lies inside the state folder: a copy of the
    /// state takes the keys along.
    InsideState(PathBuf),
    /// Each beside its sealed file, in the state folder.
    InState(Why),
}

/// Why the keys are kept in the state folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Why {
    /// No folder was named.
    NotNamed,
    /// What was named does not start at the root, so where it leads depends
    /// on where the server was started.
    NotWhole(PathBuf),
    /// No folder is at the path named.
    NoFolder(PathBuf),
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotNamed => write!(f, "{SETTING} names no folder"),
            Self::NotWhole(named) => write!(
                f,
                "{SETTING} names {}, which does not start at the root",
                named.display()
            ),
            Self::NoFolder(named) => write!(
                f,
                "{SETTING} names {}, where no folder is, and the server makes none",
                named.display()
            ),
        }
    }
}

/// Where a server whose state is under `state_root` keeps the keys, when its
/// operator answered `named` for the folder. An empty answer names none.
pub(crate) fn decide(named: &Path, state_root: &Path) -> Kept {
    if named.as_os_str().is_empty() {
        return Kept::InState(Why::NotNamed);
    }
    if !named.is_absolute() {
        return Kept::InState(Why::NotWhole(named.to_owned()));
    }
    if !named.is_dir() {
        return Kept::InState(Why::NoFolder(named.to_owned()));
    }
    // As each folder is reached, so that a link or a `..` on the way does
    // not hide that one lies in the other.
    let reached = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if reached(named).starts_with(reached(state_root)) {
        Kept::InsideState(named.to_owned())
    } else {
        Kept::Apart(named.to_owned())
    }
}

impl Kept {
    /// The folder the keys are kept under, when they are not kept beside
    /// their sealed files.
    pub(crate) fn folder(&self) -> Option<&Path> {
        match self {
            Self::Apart(folder) | Self::InsideState(folder) => Some(folder),
            Self::InState(_) => None,
        }
    }

    /// Say in the log where the keys are kept, for a server whose state is
    /// under `state_root`. A line names folders and never a key. It is
    /// written as the server's own, which an operator's log shows from
    /// `info` on: that the keys are kept apart is said as plainly as that
    /// they are not.
    pub(crate) fn say(&self, state_root: &Path) {
        let state = state_root.display();
        match self {
            Self::Apart(keys) => tracing::info!(
                target: LOG,
                keys = %keys.display(),
                %state,
                "the keys of people's sealed stores are kept apart from the state folder"
            ),
            Self::InsideState(keys) => tracing::warn!(
                target: LOG,
                keys = %keys.display(),
                %state,
                "the keys of people's sealed stores are kept inside the state folder, so a copy of the state takes them along: {SETTING} names a folder outside it"
            ),
            Self::InState(why) => tracing::warn!(
                target: LOG,
                %state,
                "the keys of people's sealed stores are kept in the state folder, each beside what it opens, so a copy of the state opens them: {why}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder keeps the keys when it was named from the root and is there.
    /// One that is not there is not made: a key written under a folder
    /// nobody made is lost with it.
    #[test]
    fn a_folder_keeps_the_keys_only_when_it_was_named_from_the_root_and_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let keys = dir.path().join("keys");

        let unnamed = decide(Path::new(""), &state);
        assert_eq!(unnamed, Kept::InState(Why::NotNamed));
        assert_eq!(unnamed.folder(), None);
        assert_eq!(
            decide(Path::new("keys"), &state),
            Kept::InState(Why::NotWhole("keys".into()))
        );

        assert_eq!(
            decide(&keys, &state),
            Kept::InState(Why::NoFolder(keys.clone()))
        );
        assert!(!keys.exists(), "deciding made the folder");
        std::fs::write(&keys, b"").unwrap();
        assert_eq!(
            decide(&keys, &state),
            Kept::InState(Why::NoFolder(keys.clone())),
            "a file is no folder"
        );
        std::fs::remove_file(&keys).unwrap();

        std::fs::create_dir(&keys).unwrap();
        let apart = decide(&keys, &state);
        assert_eq!(apart, Kept::Apart(keys.clone()));
        assert_eq!(apart.folder(), Some(keys.as_path()));
    }

    /// A folder inside the state still keeps the keys, which may lie there
    /// from before, and is told from one outside it however it is reached:
    /// a copy of the state takes those keys along.
    #[test]
    fn a_folder_inside_the_state_is_told_from_one_outside_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let inside = state.join("keys");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir(dir.path().join("elsewhere")).unwrap();

        let kept = decide(&inside, &state);
        assert_eq!(kept, Kept::InsideState(inside.clone()));
        assert_eq!(kept.folder(), Some(inside.as_path()));
        assert_eq!(decide(&state, &state), Kept::InsideState(state.clone()));
        let around = dir
            .path()
            .join("elsewhere")
            .join("..")
            .join("state")
            .join("keys");
        assert_eq!(decide(&around, &state), Kept::InsideState(around.clone()));
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&inside, &link).unwrap();
            assert_eq!(decide(&link, &state), Kept::InsideState(link.clone()));
        }

        // A folder beside the state whose name begins as the state's does
        // lies outside it.
        let beside = dir.path().join("state-keys");
        std::fs::create_dir(&beside).unwrap();
        assert_eq!(decide(&beside, &state), Kept::Apart(beside.clone()));
    }
}
