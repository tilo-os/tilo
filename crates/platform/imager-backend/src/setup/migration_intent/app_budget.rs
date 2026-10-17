//! Source-bound conservative independent application-state capacity. Original
//! shared documents are never traversed or included. Download growth is unknown.
use super::*;
use crate::setup::{
    flatpak_transfer::{self, Runner},
    migration_review::SourceInventory,
};
use sha2::Digest;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Budget {
    schema: u32,
    #[serde(default)]
    selected_only: bool,
    trees: Vec<Tree>,
    required_target_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Tree {
    path: NativePath,
    revision: Option<[u8; 32]>,
    bytes: u64,
    entries: u64,
    deployment: bool,
}
fn sum(a: u64, b: u64) -> io::Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| error("Application storage estimate overflow"))
}
fn inventory(path: &Path, deployment: bool) -> io::Result<Tree> {
    let parent = path
        .parent()
        .ok_or_else(|| error("Application source has no parent"))?;
    let parent = PinnedDirectory::open_absolute(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| error("Application source has no name"))?;
    let metadata = match crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(name)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !deployment => {
            parent.validate()?;
            return Ok(Tree {
                path: NativePath::encode(path),
                revision: None,
                bytes: 0,
                entries: 0,
                deployment,
            });
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err(error("Application source is not a real directory"));
    }
    let pin = parent.open_child(name, &metadata)?;
    let review = SourceInventory::capture_home(&pin)?;
    let summary = review.summary()?;
    if summary.special_files > 0 {
        return Err(error("Application data contains unsupported special files"));
    }
    Ok(Tree {
        path: NativePath::encode(path),
        revision: Some(review.revision()?),
        bytes: summary.bytes,
        entries: u64::try_from(review.entry_count()).map_err(io::Error::other)?,
        deployment,
    })
}
impl Budget {
    pub(super) fn capture(home: &Path, selected: &[InstalledRef]) -> io::Result<Self> {
        Self::capture_with(home, selected, &NativeRunner)
    }
    pub(super) fn capture_selected(home: &Path, selected: &[InstalledRef]) -> io::Result<Self> {
        Self::capture_scoped(home, selected, &NativeRunner, true)
    }
    fn capture_with(
        home: &Path,
        selected: &[InstalledRef],
        runner: &impl Runner,
    ) -> io::Result<Self> {
        Self::capture_scoped(home, selected, runner, false)
    }
    fn capture_scoped(
        home: &Path,
        selected: &[InstalledRef],
        runner: &impl Runner,
        selected_only: bool,
    ) -> io::Result<Self> {
        let mut trees = Vec::new();
        let paths: std::collections::BTreeSet<PathBuf> = if selected_only {
            selected
                .iter()
                .map(|reference| {
                    reference
                        .app_id()
                        .ok_or_else(|| error("Selected app has no data ID"))
                        .map(|id| home.join(".var/app").join(id))
                })
                .collect::<io::Result<_>>()?
        } else {
            [home.join(".local/share/flatpak"), home.join(".var/app")]
                .into_iter()
                .collect()
        };
        for path in paths {
            // A missing parent chain is absence, never permission to follow a
            // symlink or inspect any unrelated original-home directory.
            match PinnedDirectory::open_absolute(
                path.parent().ok_or_else(|| error("Missing app parent"))?,
            ) {
                Ok(_) => trees.push(inventory(&path, false)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => trees.push(Tree {
                    path: NativePath::encode(&path),
                    revision: None,
                    bytes: 0,
                    entries: 0,
                    deployment: false,
                }),
                Err(error) => return Err(error),
            }
        }
        if selected_only
            || selected.iter().any(|reference| {
                matches!(
                    reference.installation,
                    flatpak_transfer::Installation::System(_)
                )
            })
        {
            let current = flatpak_transfer::discover(runner, home).map_err(io::Error::other)?;
            let roots = flatpak_transfer::installation_roots(runner, home, &current)
                .map_err(io::Error::other)?;
            let mut paths = std::collections::BTreeSet::new();
            let required = if selected_only {
                // Share native runtime/conditional-extension policy with export.
                // Unrelated installed runtimes are not part of selected copying.
                flatpak_transfer::portable::review_selected_dependencies(
                    runner, home, &current, selected,
                )
                .map_err(io::Error::other)?
            } else {
                // Whole-state budgeting retains its conservative system scope.
                selected
                    .iter()
                    .filter(|reference| {
                        matches!(
                            reference.installation,
                            flatpak_transfer::Installation::System(_)
                        )
                    })
                    .chain(current.refs.iter().filter(|reference| {
                        reference.reference.starts_with("runtime/")
                            && matches!(
                                reference.installation,
                                flatpak_transfer::Installation::System(_)
                            )
                    }))
                    .cloned()
                    .collect()
            };
            for reference in &required {
                if !current.refs.contains(reference) {
                    return Err(error(
                        "Selected application source changed before budgeting",
                    ));
                }
                let location = runner
                    .run(
                        home,
                        &[
                            "info".into(),
                            reference.installation.argument(),
                            "--show-location".into(),
                            reference.reference.clone(),
                        ],
                    )
                    .map_err(io::Error::other)?;
                let path = PathBuf::from(location.trim_end_matches('\n'));
                let root = roots
                    .get(&reference.installation)
                    .ok_or_else(|| error("Selected app root missing"))?;
                let prefix = root.join(&reference.reference);
                let suffix = path
                    .strip_prefix(&prefix)
                    .map_err(|_| error("Selected deployment escaped its installation"))?;
                if suffix.components().count() != 1
                    || suffix.as_os_str().len() != 64
                    || !suffix
                        .to_str()
                        .is_some_and(|text| text.bytes().all(|byte| byte.is_ascii_hexdigit()))
                {
                    return Err(error("Selected deployment has no exact commit directory"));
                }
                if paths.insert(path.clone()) {
                    trees.push(inventory(&path, true)?);
                }
            }
        }
        if trees.len() > 4096 {
            return Err(error(
                "Application estimate exceeds bounded installation count",
            ));
        }
        if selected_only {
            let ids: std::collections::BTreeSet<_> =
                selected.iter().filter_map(InstalledRef::app_id).collect();
            for id in ids {
                let path = home.join(".local/share/flatpak/overrides").join(id);
                let revision = match PinnedDirectory::open_absolute(
                    path.parent()
                        .ok_or_else(|| error("App override parent absent"))?,
                ) {
                    Ok(parent) => {
                        let mut options = OpenOptions::new();
                        options.read(true).follow(FollowSymlinks::No).nonblock(true);
                        match cap_primitives::fs::open(&parent.file, Path::new(id), &options) {
                            Ok(mut file) => {
                                use std::os::unix::fs::MetadataExt;
                                let before = file.metadata()?;
                                if !before.is_file() || before.len() > 128 * 1024 {
                                    return Err(error("Selected app override is unsupported"));
                                }
                                let mut bytes = Vec::new();
                                (&mut file).take(128 * 1024 + 1).read_to_end(&mut bytes)?;
                                let after = file.metadata()?;
                                if bytes.len() as u64 != before.len()
                                    || before.dev() != after.dev()
                                    || before.ino() != after.ino()
                                    || before.len() != after.len()
                                    || before.ctime() != after.ctime()
                                    || before.ctime_nsec() != after.ctime_nsec()
                                {
                                    return Err(error(
                                        "Selected app override changed during budget review",
                                    ));
                                }
                                parent.validate()?;
                                Some(sha2::Sha256::digest(&bytes).into())
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
                trees.push(Tree {
                    path: NativePath::encode(&path),
                    revision,
                    bytes: 128 * 1024,
                    entries: 1,
                    deployment: false,
                });
            }
        }
        let required_target_bytes = Self::required(&trees)?;
        Ok(Self {
            schema: 1,
            selected_only,
            trees,
            required_target_bytes,
        })
    }
    fn required(trees: &[Tree]) -> io::Result<u64> {
        let mut bytes = 0;
        let mut entries = 0;
        for tree in trees {
            bytes = sum(
                bytes,
                tree.bytes
                    .checked_mul(if tree.deployment { 2 } else { 1 })
                    .ok_or_else(|| error("App deployment estimate overflow"))?,
            )?;
            entries = sum(entries, tree.entries)?;
        }
        if entries > 1_000_000 {
            return Err(error("Application estimate exceeds bounded review"));
        }
        let overhead = entries
            .checked_mul(8192)
            .ok_or_else(|| error("App metadata estimate overflow"))?;
        let extra = sum(sum(bytes, overhead)?, (bytes / 4).max(2 << 30))?;
        let extra = extra
            .checked_mul(100)
            .ok_or_else(|| error("App reserve estimate overflow"))?
            .div_ceil(94);
        sum(staging::MIN_TARGET_BYTES, extra)
    }
    pub(super) fn renew(&self, home: &Path, selected: &[InstalledRef]) -> io::Result<()> {
        if self.schema != 1
            || self.trees.len() > 4096
            || Self::required(&self.trees)? != self.required_target_bytes
        {
            return Err(error("Application storage estimate is invalid"));
        }
        let current = if self.selected_only {
            Self::capture_selected(home, selected)?
        } else {
            Self::capture(home, selected)?
        };
        if current != *self {
            return Err(error("Application data changed; review storage again"));
        }
        Ok(())
    }
    pub(super) fn minimum(&self) -> u64 {
        self.required_target_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_app_inventory_excludes_shared_documents_and_refuses_changed_data() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".var/app/org.example.Chat/data")).unwrap();
        let state = root.path().join(".var/app/org.example.Chat/data/database");
        std::fs::write(&state, b"app data").unwrap();
        let initial = Budget::capture(root.path(), &[]).unwrap();
        let document = std::fs::File::create(root.path().join("large original document")).unwrap();
        document.set_len(100 << 30).unwrap();
        assert_eq!(Budget::capture(root.path(), &[]).unwrap(), initial);
        initial.renew(root.path(), &[]).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&state)
            .unwrap()
            .set_len(20 << 30)
            .unwrap();
        let larger = Budget::capture(root.path(), &[]).unwrap();
        assert!(larger.minimum() > initial.minimum() + (20 << 30));
        assert!(initial.renew(root.path(), &[]).is_err());
        assert_eq!(document.metadata().unwrap().len(), 100 << 30);
    }
    #[test]
    fn absent_app_data_retains_only_os_minimum_and_explicit_growth_headroom() {
        let root = tempfile::tempdir().unwrap();
        let budget = Budget::capture(root.path(), &[]).unwrap();
        assert_eq!(
            budget.minimum(),
            staging::MIN_TARGET_BYTES + ((2_u64 << 30) * 100).div_ceil(94)
        );
        budget.renew(root.path(), &[]).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn real_runtime_socket_is_zero_bytes_while_link_roots_refuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(".var/app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("data"), b"keep data").unwrap();
        let socket = std::os::unix::net::UnixListener::bind(app.join("socket")).unwrap();
        let budget = Budget::capture(root.path(), &[]).unwrap();
        assert_eq!(budget.trees.iter().map(|tree| tree.bytes).sum::<u64>(), 9);
        drop(socket);
        std::fs::remove_file(app.join("socket")).unwrap();
        std::fs::remove_dir_all(&app).unwrap();
        std::os::unix::fs::symlink(root.path(), &app).unwrap();
        assert!(Budget::capture(root.path(), &[]).is_err());
    }
    struct SystemApps {
        root: PathBuf,
    }
    impl Runner for SystemApps {
        fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
            match args.first().map(String::as_str) {
                Some("list") if args.iter().any(|arg| arg == "--app") => {
                    Ok("org.example.Chat/x86_64/stable\tflathub\twork\tChat\n".into())
                }
                Some("list") => {
                    Ok("org.example.Runtime/x86_64/stable\tflathub\twork\tRuntime\n".into())
                }
                Some("remotes") if args.iter().any(|arg| arg == "--user") => Ok(String::new()),
                Some("remotes") => Ok("flathub\thttps://example.org/repo/\t\n".into()),
                Some("info") if args.iter().any(|arg| arg == "--show-runtime") => {
                    Ok("org.example.Runtime/x86_64/stable\n".into())
                }
                Some("info") if args.iter().any(|arg| arg == "--show-extensions") => {
                    Ok(format!("{}\n", args.last().ok_or("Missing reference")?))
                }
                Some("info") => Ok(self
                    .root
                    .join(args.last().ok_or("Missing reference")?)
                    .join("a".repeat(64))
                    .to_string_lossy()
                    .into_owned()),
                _ => Err("Unexpected app-budget query".into()),
            }
        }
    }
    struct SelectedClosure(SystemApps);
    impl Runner for SelectedClosure {
        fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
            if args.first().map(String::as_str) == Some("list")
                && !args.iter().any(|arg| arg == "--app")
            {
                return Ok("org.example.Runtime/x86_64/stable\tflathub\twork\tRuntime\norg.example.Chat.Locale/x86_64/stable\tflathub\twork\tLocale\norg.example.Unrelated/x86_64/stable\tflathub\twork\tUnrelated\n".into());
            }
            if args.iter().any(|arg| arg == "--show-extensions")
                && args.last().is_some_and(|arg| arg.starts_with("app/"))
            {
                return Ok(format!(
                    "{}\n Extension: runtime/org.example.Chat.Locale/x86_64/stable\n Installation: system (work)\n",
                    args.last().unwrap()
                ));
            }
            self.0.run(home, args)
        }
    }
    #[test]
    fn selected_budget_uses_export_closure_without_unrelated_runtime_capacity() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let installation = root.path().join("installation");
        std::fs::create_dir(&home).unwrap();
        for (reference, bytes) in [
            ("app/org.example.Chat/x86_64/stable", 1024_u64),
            ("runtime/org.example.Runtime/x86_64/stable", 1 << 30),
            ("runtime/org.example.Chat.Locale/x86_64/stable", 64 << 20),
            ("runtime/org.example.Unrelated/x86_64/stable", 16 << 30),
        ] {
            let path = installation.join(reference).join("a".repeat(64));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::File::create(path.join("payload"))
                .unwrap()
                .set_len(bytes)
                .unwrap();
        }
        let runner = SelectedClosure(SystemApps { root: installation });
        let selected = InstalledRef {
            reference: "app/org.example.Chat/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: flatpak_transfer::Installation::System("work".into()),
            name: "Chat".into(),
        };
        let budget = Budget::capture_scoped(&home, &[selected.clone()], &runner, true).unwrap();
        let deployments: Vec<_> = budget.trees.iter().filter(|tree| tree.deployment).collect();
        assert_eq!(deployments.len(), 3);
        assert!(deployments.iter().any(|tree| tree.bytes == 1 << 30));
        assert!(deployments.iter().any(|tree| tree.bytes == 64 << 20));
        assert!(!deployments.iter().any(|tree| tree.bytes == 16 << 30));
        assert!(budget.minimum() < staging::MIN_TARGET_BYTES + (5 << 30));
        let mut current = flatpak_transfer::discover(&runner, &home).unwrap();
        current
            .refs
            .retain(|item| !item.reference.contains("Chat.Locale"));
        assert!(
            flatpak_transfer::portable::review_selected_dependencies(
                &runner,
                &home,
                &current,
                &[selected.clone()]
            )
            .is_err()
        );
        let mut wrong_scope = selected;
        wrong_scope.installation = flatpak_transfer::Installation::User;
        assert!(
            flatpak_transfer::portable::review_selected_dependencies(
                &runner,
                &home,
                &current,
                &[wrong_scope]
            )
            .is_err()
        );
    }
    #[test]
    fn selected_small_app_budget_includes_large_original_system_runtime() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let installation = root.path().join("installation");
        std::fs::create_dir(&home).unwrap();
        for (reference, bytes) in [
            ("app/org.example.Chat/x86_64/stable", 1024_u64),
            ("runtime/org.example.Runtime/x86_64/stable", 1_u64 << 30),
        ] {
            let path = installation.join(reference).join("a".repeat(64));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::File::create(path.join("payload"))
                .unwrap()
                .set_len(bytes)
                .unwrap();
        }
        let selected = InstalledRef {
            reference: "app/org.example.Chat/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: flatpak_transfer::Installation::System("work".into()),
            name: "Chat".into(),
        };
        let budget =
            Budget::capture_with(&home, &[selected], &SystemApps { root: installation }).unwrap();
        assert!(
            budget
                .trees
                .iter()
                .any(|tree| tree.deployment && tree.bytes == 1_u64 << 30)
        );
        assert!(budget.minimum() > staging::MIN_TARGET_BYTES + (4 << 30));
    }
    #[test]
    fn selected_copy_budget_excludes_unselected_state_and_shared_documents() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let installation = root.path().join("installation");
        std::fs::create_dir_all(home.join(".var/app/org.example.Chat/data")).unwrap();
        std::fs::create_dir_all(home.join(".var/app/org.example.Unselected/data")).unwrap();
        std::fs::write(
            home.join(".var/app/org.example.Chat/data/state"),
            b"selected state",
        )
        .unwrap();
        let unselected = home.join(".var/app/org.example.Unselected/data/state");
        std::fs::File::create(&unselected)
            .unwrap()
            .set_len(1 << 30)
            .unwrap();
        for reference in [
            "app/org.example.Chat/x86_64/stable",
            "runtime/org.example.Runtime/x86_64/stable",
        ] {
            let path = installation.join(reference).join("a".repeat(64));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("payload"), b"selected installation").unwrap();
        }
        let selected = InstalledRef {
            reference: "app/org.example.Chat/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: flatpak_transfer::Installation::System("work".into()),
            name: "Chat".into(),
        };
        let runner = SystemApps { root: installation };
        let first = Budget::capture_scoped(&home, &[selected.clone()], &runner, true).unwrap();
        assert!(first.selected_only);
        assert!(
            first.trees.iter().any(
                |tree| tree.path == NativePath::encode(&home.join(".var/app/org.example.Chat"))
            )
        );
        assert!(
            !first.trees.iter().any(|tree| tree.path
                == NativePath::encode(&home.join(".var/app/org.example.Unselected")))
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&unselected)
            .unwrap()
            .set_len(100 << 30)
            .unwrap();
        std::fs::File::create(home.join("shared document"))
            .unwrap()
            .set_len(100 << 30)
            .unwrap();
        assert_eq!(
            Budget::capture_scoped(&home, &[selected.clone()], &runner, true).unwrap(),
            first
        );
        let removed = Budget::capture_scoped(&home, &[], &runner, true).unwrap();
        assert!(removed.minimum() < first.minimum());
        std::fs::write(
            home.join(".var/app/org.example.Chat/data/state"),
            b"changed selected state",
        )
        .unwrap();
        assert_ne!(
            Budget::capture_scoped(&home, &[selected], &runner, true).unwrap(),
            first
        );
        assert_eq!(std::fs::metadata(unselected).unwrap().len(), 100 << 30);
        assert_eq!(
            std::fs::metadata(home.join("shared document"))
                .unwrap()
                .len(),
            100 << 30
        );
    }
}
