//! A workspace's environments, branched into another workspace, as the person's
//! `workspace.branch` asks.
//!
//! Each environment of the source becomes one of the branch under the same key: its
//! record forked with its history ([`Records::branch_into`]), its home and grants moved
//! to the same places in the branch's tree, and what it keeps outside its record (its
//! legacy home, its guest's `/var`) copied there. Its id is the branch's
//! (`<branch>/<key>`), so its sessions and agent door are the branch's. The source is
//! read and never changed, and images are shared already.

use std::path::{Path, PathBuf};

use tilo_files::native::tree::{Strategy, clone_tree};

use crate::records::Records;
use crate::store::Store;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Branched {
    pub environments: u64,
    pub files: u64,
    pub bytes: u64,
}

/// Branch the environments of the workspace `from` is of into the one `to` is of
/// ([`Store::for_workspace`]).
///
/// # Errors
/// The two are one workspace, the branch has an environment of the same key already, or a record
/// or a file could not be read or written.
pub fn branch_environments(from: &Store, to: &Store) -> Result<Branched, String> {
    if from.workspace_id() == to.workspace_id() {
        return Err("a workspace is branched into another".to_owned());
    }
    let source = Records::of(from);
    let target = Records::of(to);
    let trees = (
        from.workspace().map(Path::to_path_buf),
        to.workspace().map(Path::to_path_buf),
    );
    let rehome = |path: &str| moved(path, &trees);
    let mut branched = Branched::default();
    for key in source.keys().map_err(|error| error.to_string())? {
        if !source
            .branch_into(&key, &target, &rehome)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        branched.environments = branched.environments.saturating_add(1);
        let kept = from.environment_dir(&key);
        if kept.is_dir() {
            let copied = clone_tree(
                &kept,
                &to.environment_dir(&key),
                Strategy::Clone,
                &mut |path| Some(path.to_path_buf()),
                &mut |_| {},
            )
            .map_err(|error| format!("environment {key}'s own files: {error}"))?;
            branched.files = branched.files.saturating_add(copied.files);
            branched.bytes = branched.bytes.saturating_add(copied.bytes);
        }
    }
    Ok(branched)
}

fn moved(path: &str, (from, to): &(Option<PathBuf>, Option<PathBuf>)) -> Option<String> {
    let (from, to) = (from.as_deref()?, to.as_deref()?);
    let rest = Path::new(path).strip_prefix(from).ok()?;
    let moved = if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    };
    Some(moved.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceId;
    use tilo_environment::{Environment, dir_grant};

    const FROM: &str = "6f1c2d3e-0000-4000-8000-000000000001";
    const TO: &str = "6f1c2d3e-0000-4000-8000-000000000002";

    fn device(root: &Path) -> Store {
        std::fs::create_dir_all(root.join("files")).unwrap();
        Store::at(root.join("ush")).with_workspace(root.join("files"))
    }

    fn workspace(store: &Store, id: &str) -> Store {
        store.for_workspace(&WorkspaceId::parse(Some(id)).unwrap())
    }

    #[test]
    fn an_environment_of_the_source_is_the_branchs_with_its_home_and_its_own_files() {
        let root = tempfile::tempdir().unwrap();
        let device = device(root.path());
        let from = workspace(&device, FROM);
        let to = workspace(&device, TO);
        let tree = from.workspace().unwrap().to_path_buf();
        std::fs::create_dir_all(tree.join("src")).unwrap();
        let outside = root.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();

        let records = Records::of(&from);
        let mut env = Environment {
            name: "Dev".to_owned(),
            ..Environment::default()
        };
        env.set_home(tree.display().to_string()).unwrap();
        env.grants
            .push(dir_grant(&tree.join("src"), false).unwrap());
        env.grants.push(dir_grant(&outside, true).unwrap());
        records.create("env-1", &env).unwrap();
        let var = from.environment_dir("env-1").join("var");
        std::fs::create_dir_all(var.join("lib/apt")).unwrap();
        std::fs::write(var.join("lib/apt/state"), b"installed").unwrap();

        let branched = branch_environments(&from, &to).unwrap();
        assert_eq!(branched.environments, 1);
        assert_eq!(branched.files, 1);

        let copy = Records::of(&to)
            .load("env-1")
            .unwrap()
            .expect("the branch's environment");
        assert_eq!(copy.name, "Dev");
        assert_eq!(
            copy.workspace, TO,
            "the branch's own, so its sessions are the branch's"
        );
        let branch_tree = to.workspace().unwrap();
        assert_eq!(Path::new(&copy.home_dir), branch_tree);
        let paths: Vec<String> = copy
            .grants
            .iter()
            .filter_map(|grant| match &grant.scope {
                collab_types::caps::Scope::FsPath(path) => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert!(paths.contains(&branch_tree.display().to_string()));
        assert!(paths.contains(&branch_tree.join("src").display().to_string()));
        assert!(
            paths.contains(&outside.display().to_string()),
            "a grant outside stays"
        );
        assert!(
            !paths.iter().any(|path| Path::new(path).starts_with(&tree)),
            "nothing of the source's files: {paths:?}"
        );
        assert_eq!(
            std::fs::read(to.environment_dir("env-1").join("var/lib/apt/state")).unwrap(),
            b"installed"
        );

        let source = records.load("env-1").unwrap().unwrap();
        assert_eq!(source.workspace, FROM);
        assert_eq!(Path::new(&source.home_dir), tree);
        std::fs::write(
            to.environment_dir("env-1").join("var/lib/apt/state"),
            b"changed",
        )
        .unwrap();
        assert_eq!(
            std::fs::read(var.join("lib/apt/state")).unwrap(),
            b"installed"
        );

        assert!(branch_environments(&from, &to).is_err());
        assert!(branch_environments(&from, &from).is_err());
    }
}
