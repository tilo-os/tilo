//! Real build sandboxes (bwrap, a user namespace, overlayfs) against the
//! fixture runtime, and against a real env-runtime tree when one is given.
//!
//! Each test returns early where the host can't create the sandbox (no
//! bwrap, user namespaces disabled), saying so.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use env_tools::{
    BuildError, Builder, CacheKey, Catalog, Entry, NoCatalog, Origin, ToolStore, rebuild_tools,
};
use std::collections::BTreeMap;
use tilo_app_package::PackageId;

use tilo_environment::{Environment, RuntimeRef, ToolBuildError, ToolBuilder};

use crate::fixture;

fn sandbox_or_skip() -> bool {
    if env_tools::sandbox::available() {
        true
    } else {
        eprintln!("skipping: this host cannot run a bwrap build sandbox");
        false
    }
}

/// A store over one runtime tree that keeps sealed trees by the hash of
/// their listing.
struct FakeStore {
    runtimes: Vec<(PackageId, PathBuf)>,
    kept: PathBuf,
    stored: Mutex<Vec<(PackageId, CacheKey, Vec<String>)>>,
}

impl FakeStore {
    fn new(runtimes: Vec<(PackageId, PathBuf)>, kept: PathBuf) -> Self {
        Self {
            runtimes,
            kept,
            stored: Mutex::new(Vec::new()),
        }
    }
}

impl ToolStore for FakeStore {
    fn runtime_tree(&self, runtime: &PackageId) -> anyhow::Result<PathBuf> {
        self.runtimes
            .iter()
            .find(|(id, _)| id == runtime)
            .map(|(_, tree)| tree.clone())
            .ok_or_else(|| anyhow::anyhow!("no runtime {runtime}"))
    }

    fn has(&self, tool: &PackageId) -> bool {
        self.stored
            .lock()
            .unwrap()
            .iter()
            .any(|(id, _, _)| id == tool)
    }

    fn store_tool(
        &self,
        tree: &Path,
        key: &CacheKey,
        requested: &[String],
    ) -> anyhow::Result<PackageId> {
        let listing = format!("{:?}{:?}", fixture::listing(tree), key.runtime);
        let id = PackageId::from_bytes(*blake3::hash(listing.as_bytes()).as_bytes());
        std::fs::create_dir_all(&self.kept)?;
        std::fs::rename(tree, self.kept.join(id.to_hex()))?;
        self.stored
            .lock()
            .unwrap()
            .push((id, key.clone(), requested.to_vec()));
        Ok(id)
    }
}

fn packages(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn a_fixture_build_seals_only_what_the_packages_added() {
    if !sandbox_or_skip() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(tree) = fixture::runtime(dir.path()) else {
        eprintln!("skipping: host programs are not under a merged /usr");
        return;
    };
    let runtime = PackageId::from_bytes([1; 32]);
    let store = FakeStore::new(vec![(runtime, tree)], dir.path().join("kept"));
    let builder = Builder::new(store, NoCatalog, dir.path().join("state"));

    let built = builder.build(&runtime, &packages(&["hello"])).unwrap();
    assert_eq!(
        built.key,
        CacheKey::new(runtime, [fixture::HELLO, fixture::LIBGREET])
    );
    let Origin::Built { report, .. } = &built.origin else {
        panic!("expected a build, got {:?}", built.origin);
    };
    assert_eq!(report.whiteouts, ["usr/share/base/obsolete"], "{report:?}");
    assert_eq!(report.shadows, ["etc/shells"]);
    for dropped in [
        "etc/hello.conf.rpmnew",
        "etc/ld.so.cache",
        "usr/lib/.build-id",
        "usr/lib/sysimage/libdnf5",
        "usr/lib/sysimage/rpm",
    ] {
        assert!(
            report.dropped.iter().any(|d| d == dropped),
            "{dropped}: {report:?}"
        );
    }

    let sealed = builder.store().kept.join(built.tool.to_hex());
    let listing = fixture::listing(&sealed);
    let files: Vec<&str> = listing
        .iter()
        .filter(|(_, (kind, ..))| *kind != 'd')
        .map(|(path, _)| path.as_str())
        .collect();
    assert_eq!(
        files,
        [
            "etc/hello.conf",
            "etc/shells",
            "usr/bin/hello",
            "usr/lib64/libgreet.so.2",
            "usr/lib64/libgreet.so.2.0",
            "usr/share/hello/greeting.txt",
        ]
    );
    let (_, mode, mtime, _) = &listing["usr/bin/hello"];
    assert_eq!((*mode, *mtime), (0o755, 0));
    assert_eq!(listing["usr/lib64/libgreet.so.2"].3, b"libgreet.so.2.0");
    assert!(!sealed.join("var").exists());

    // The same request again is a local hit, without a second build.
    let again = builder.build(&runtime, &packages(&["hello"])).unwrap();
    assert_eq!(again.tool, built.tool);
    assert_eq!(again.origin, Origin::Local);
    assert_eq!(builder.store().stored.lock().unwrap().len(), 1);

    env_tools::sandbox::remove_tree(dir.path()).unwrap();
}

#[test]
fn two_fixture_builds_are_identical() {
    if !sandbox_or_skip() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(tree) = fixture::runtime(dir.path()) else {
        return;
    };
    let nevras = packages(&[fixture::HELLO, fixture::LIBGREET]);
    let mut listings = Vec::new();
    for n in 0..2 {
        let out = dir.path().join(format!("out{n}"));
        let sealed = env_tools::builder::build_tree(
            &tree,
            &nevras,
            &dir.path().join(format!("scratch{n}")),
            &out,
            None,
            None,
            std::time::Duration::from_secs(120),
        )
        .unwrap();
        assert_eq!(sealed.nevras, nevras);
        listings.push(fixture::listing(&out));
        // Builds a second later must not differ by time.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    assert_eq!(listings[0], listings[1]);
    env_tools::sandbox::remove_tree(dir.path()).unwrap();
}

#[test]
fn a_catalog_hit_skips_the_build() {
    if !sandbox_or_skip() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(tree) = fixture::runtime(dir.path()) else {
        return;
    };
    let runtime = PackageId::from_bytes([2; 32]);
    let published = PackageId::from_bytes([0xcc; 32]);
    let mut catalog = Catalog::default();
    catalog.insert(Entry {
        tool: published,
        key: CacheKey::new(runtime, [fixture::LIBGREET, fixture::HELLO]),
        requested: packages(&["hello"]),
    });
    let store = FakeStore::new(vec![(runtime, tree)], dir.path().join("kept"));
    let builder = Builder::new(store, catalog, dir.path().join("state"));
    let built = builder.build(&runtime, &packages(&["hello"])).unwrap();
    assert_eq!((built.tool, built.origin), (published, Origin::Catalog));
    assert!(builder.store().stored.lock().unwrap().is_empty());
    env_tools::sandbox::remove_tree(dir.path()).unwrap();
}

#[test]
fn requests_dnf_rejects_fail_before_building() {
    if !sandbox_or_skip() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(tree) = fixture::runtime(dir.path()) else {
        return;
    };
    let runtime = PackageId::from_bytes([3; 32]);
    let store = FakeStore::new(vec![(runtime, tree)], dir.path().join("kept"));
    let builder = Builder::new(store, NoCatalog, dir.path().join("state"));
    assert!(matches!(
        builder.build(&runtime, &packages(&["nosuch"])),
        Err(BuildError::Resolve(env_tools::resolve::ResolveError::NoMatch(names))) if names == ["nosuch"]
    ));
    assert!(matches!(
        builder.build(&runtime, &packages(&["base"])),
        Err(BuildError::AlreadyInRuntime(_))
    ));
    assert!(matches!(
        builder.build(&runtime, &[]),
        Err(BuildError::Empty)
    ));
    assert!(builder.store().stored.lock().unwrap().is_empty());
    env_tools::sandbox::remove_tree(dir.path()).unwrap();
}

#[test]
fn set_runtime_rebuilds_every_tool_or_none() {
    if !sandbox_or_skip() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(tree) = fixture::runtime(dir.path()) else {
        return;
    };
    let (old, new) = (
        PackageId::from_bytes([4; 32]),
        PackageId::from_bytes([5; 32]),
    );
    let store = FakeStore::new(
        vec![(old, tree.clone()), (new, tree)],
        dir.path().join("kept"),
    );
    let builder = Builder::new(store, NoCatalog, dir.path().join("state"));

    // `pkg install hello` on the old runtime, through the record's seam.
    let mut env = Environment::new("work", "2026.10", "x86_64", &old);
    let mut progress = Vec::new();
    let tool = ToolBuilder::build(&builder, &old, &packages(&["hello"]), &mut progress).unwrap();
    env.add_tool(packages(&["hello"]), "x86_64".into(), tool.to_string())
        .unwrap();
    let progress = String::from_utf8(progress).unwrap();
    assert!(progress.contains("resolving hello"), "{progress}");
    assert!(
        progress.contains("Complete!"),
        "dnf's output reaches the shell: {progress}"
    );

    let rebuilt =
        rebuild_tools(&builder, "x86_64", &new, &env.tools, &mut std::io::sink()).unwrap();
    let release = RuntimeRef {
        release: "2026.11".into(),
        images: BTreeMap::from([("x86_64".to_owned(), new.to_string())]),
    };
    env.set_runtime(release, rebuilt).unwrap();
    assert_eq!(env.runtime_image("x86_64").unwrap(), new);
    assert_eq!(env.tools.len(), 1);
    assert_eq!(env.tools[0].packages, ["hello"]);
    let image = env.tools[0].images.get("x86_64").unwrap();
    assert_ne!(*image, tool.to_string(), "a tool is bound to its runtime");

    let mut broken = env.clone();
    broken
        .add_tool(
            packages(&["nosuch"]),
            "x86_64".into(),
            PackageId::from_bytes([9; 32]).to_string(),
        )
        .unwrap();
    let err = rebuild_tools(
        &builder,
        "x86_64",
        &old,
        &broken.tools,
        &mut std::io::sink(),
    )
    .unwrap_err();
    assert_eq!(err.packages, ["nosuch"]);
    assert!(matches!(err.source, ToolBuildError::UnknownPackage(ref n) if n == "nosuch"));
    env_tools::sandbox::remove_tree(dir.path()).unwrap();
}

/// A real build against an env-runtime tree: `TILO_ENV_RUNTIME_TREE` names
/// the materialised runtime (it needs the network for the downloads).
#[test]
fn a_real_runtime_builds_a_small_package() {
    let Some(tree) = std::env::var_os("TILO_ENV_RUNTIME_TREE").map(PathBuf::from) else {
        eprintln!("skipping: TILO_ENV_RUNTIME_TREE names no env-runtime tree");
        return;
    };
    if !sandbox_or_skip() {
        return;
    }
    let dir = tempfile::tempdir_in(
        std::env::var_os("TILO_ENV_TOOLS_SCRATCH").map_or_else(std::env::temp_dir, PathBuf::from),
    )
    .unwrap();
    let runtime = PackageId::from_bytes([6; 32]);
    let store = FakeStore::new(vec![(runtime, tree)], dir.path().join("kept"));
    let builder = Builder::new(store, NoCatalog, dir.path().join("state"));
    let started = std::time::Instant::now();
    let built = builder.build(&runtime, &packages(&["tree"])).unwrap();
    eprintln!(
        "built {:?} in {:?}: {:?}",
        built.key.nevras,
        started.elapsed(),
        built.origin
    );
    assert!(built.key.nevras.iter().any(|n| n.starts_with("tree-")));
    let sealed = builder.store().kept.join(built.tool.to_hex());
    assert!(sealed.join("usr/bin/tree").is_file());
    assert!(!sealed.join("usr/lib/sysimage/rpm").exists());
    env_tools::sandbox::remove_tree(dir.path()).unwrap();
}
