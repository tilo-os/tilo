//! The pre-built system view exposes exactly what per-path binds did.
//!
//! A view replaces ~40 per-path `--ro-bind-try` operations with a few binds
//! of a tree assembled once (`launcher::sandbox_view`). That is only
//! acceptable if the app cannot tell, so this test does not compare argvs: it
//! starts real sandboxes both ways and lists, from inside, every path the app
//! can see with its type, mode, owner and read-only flag, for representative
//! launches. The listings must be identical except for the directories
//! bubblewrap creates as parents of the binds (`/usr`, `/etc`, ...): per path
//! they sit in bubblewrap's writable root tmpfs, in the view they are `0755`
//! and read-only — the app can still read and traverse them and can no
//! longer create entries in them. Anything else differing fails.
//!
//! Building a view needs mount privilege, so the comparison runs in a user
//! namespace of its own (`unshare`), where it builds the view with the same
//! code the root service runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use launcher::capabilities::CapabilityPlan;
use launcher::manifest::{Entrypoint, Fs, ManifestView, Net, Sandbox};
use launcher::sandbox_view::{SystemView, Variant, view_read_paths};

const INNER_ENV: &str = "TILO_SYSTEM_VIEW_INNER";
const LIST_ENV: &str = "TILO_SYSTEM_VIEW_LIST";
const LISTER: &str = "/tilo-view-lister";

#[test]
fn the_view_exposes_exactly_what_per_path_binds_did() {
    let exe = std::env::current_exe().unwrap();
    let mut unshare = Command::new("unshare");
    // Root (a CI container) already holds mount privilege; anyone else
    // takes it in a user namespace of their own.
    if !rustix::process::geteuid().is_root() {
        unshare.args(["--user", "--map-root-user"]);
    }
    let output = unshare
        .args(["--mount", "--propagation", "private"])
        .arg(&exe)
        .args([
            "--exact",
            "system_view::compare_inside_a_user_namespace",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INNER_ENV, "1")
        .output()
        .expect("unshare(1) runs the comparison in a user namespace of its own");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed"),
        "the comparison failed or never ran:\n{stdout}\n{stderr}"
    );
}

#[test]
#[ignore = "entered by the_view_exposes_exactly_what_per_path_binds_did, in a user namespace"]
fn compare_inside_a_user_namespace() {
    assert_eq!(
        std::env::var(INNER_ENV).as_deref(),
        Ok("1"),
        "run through the_view_exposes_exactly_what_per_path_binds_did"
    );
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("views");
    let built = launcher::sandbox_view::build(&root).unwrap();
    let view = SystemView::open(&root, 0)
        .unwrap()
        .expect("the view just built is current");
    assert!(root.join(&built.generation).is_dir());

    let payload = work.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), b"").unwrap();
    let runtime = fake_runtime(work.path());
    let grants = work.path().join("grants");
    for dir in ["ro", "rw/.private"] {
        std::fs::create_dir_all(grants.join(dir)).unwrap();
    }

    // A graphical app on a runtime bundle, no network: the runtime's
    // loader is bound before the profile (and hidden by it), a GPU-style
    // narrowing bind lands inside a profile directory after it.
    let mut inside = Vec::new();
    if let Some(file) = a_file_inside_the_profile() {
        inside.extend(["--ro-bind".to_string(), file.clone(), file]);
    }
    compare(&Launch {
        view: &view,
        manifest: manifest("off"),
        payload: &payload,
        runtime: Some((runtime.as_path(), "2026.10+test")),
        capabilities: CapabilityPlan::default(),
        extra: inside,
        variant: Variant::Plain,
    });

    // Unrestricted network and the user's files: the network view, the
    // resolver files fresh per launch, grants with a private store masked.
    compare(&Launch {
        view: &view,
        manifest: manifest("host"),
        payload: &payload,
        runtime: None,
        capabilities: CapabilityPlan {
            ro_paths: vec![grants.join("ro")],
            rw_paths: vec![grants.join("rw")],
            pinned_dirs: vec![grants.join("rw")],
            masked_dirs: vec![grants.join("rw/.private")],
            ..CapabilityPlan::default()
        },
        extra: Vec::new(),
        variant: Variant::Net,
    });

    // The control panel's host binaries: a path the view does not have. The
    // launch keeps its per-path binds, untouched.
    let mut argv = argv_for(&Launch {
        view: &view,
        manifest: manifest("off"),
        payload: &payload,
        runtime: None,
        capabilities: CapabilityPlan::default(),
        extra: ["--ro-bind", "/usr/bin", "/usr/bin"]
            .map(String::from)
            .to_vec(),
        variant: Variant::Plain,
    });
    let before = argv.clone();
    assert!(view.compose(&mut argv).is_err());
    assert_eq!(argv, before);
}

#[test]
#[ignore = "the program the comparison runs inside each sandbox"]
fn list_the_visible_filesystem() {
    assert_eq!(
        std::env::var(LIST_ENV).as_deref(),
        Ok("1"),
        "run inside a sandbox by compare_inside_a_user_namespace"
    );
    let mut lines = Vec::new();
    walk(Path::new("/"), 0, &mut lines);
    println!("{}", lines.join("\n"));
}

/// One line per visible path: `VIEW<TAB>path<TAB>type mode uid ro target`.
fn walk(path: &Path, depth: usize, lines: &mut Vec<String>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let kind = meta.file_type();
    let type_char = if kind.is_dir() {
        'd'
    } else if kind.is_symlink() {
        'l'
    } else if kind.is_file() {
        'f'
    } else {
        'o'
    };
    let target = if kind.is_symlink() {
        std::fs::read_link(path)
            .map(|t| t.display().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let read_only = if kind.is_symlink() {
        String::from("-")
    } else {
        read_only(path).to_string()
    };
    lines.push(format!(
        "VIEW\t{}\t{type_char} {:o} {} {read_only} {target}",
        path.display(),
        meta.permissions().mode() & 0o7777,
        meta.uid(),
    ));
    if !kind.is_dir() {
        return;
    }
    // A process's own /proc differs per run; /sys is the same live bind both
    // ways and only its top level is listed.
    if path == Path::new("/proc") || (path.starts_with("/sys") && depth >= 2) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    children.sort();
    for child in children {
        walk(&child, depth.saturating_add(1), lines);
    }
}

fn read_only(path: &Path) -> bool {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: zeroed is a valid statvfs; a live C string.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let result = unsafe { libc::statvfs(path.as_ptr(), &raw mut stat) };
    result == 0 && stat.f_flag & libc::ST_RDONLY != 0
}

struct Launch<'a> {
    view: &'a SystemView,
    manifest: ManifestView,
    payload: &'a Path,
    runtime: Option<(&'a Path, &'a str)>,
    capabilities: CapabilityPlan,
    extra: Vec<String>,
    variant: Variant,
}

/// The launcher's argv for `launch`, with the lister as its program.
fn argv_for(launch: &Launch<'_>) -> Vec<String> {
    let mut argv = launcher::sandbox::build_bwrap_argv(
        &launch.manifest,
        launch.payload,
        launch.runtime,
        &[],
        None,
        None,
        &launch.capabilities,
    );
    argv.pop();
    argv.extend(launch.extra.iter().cloned());
    let exe = std::env::current_exe().unwrap();
    argv.extend([
        "--ro-bind".to_string(),
        exe.display().to_string(),
        LISTER.to_string(),
    ]);
    argv.extend(["--setenv", LIST_ENV, "1", LISTER].map(String::from));
    argv
}

/// Run `argv` under bubblewrap with the launcher's account file on fd 100.
fn listing(argv: &[String]) -> BTreeMap<String, String> {
    let account = tempfile::tempfile().unwrap();
    std::io::Write::write_all(
        &mut &account,
        b"app:x:0:0:Sandbox application:/run/tilo/app:/sbin/nologin\n",
    )
    .unwrap();
    // SAFETY: a live descriptor; F_DUPFD leaves the copy inheritable.
    let fd = unsafe { libc::fcntl(account.as_raw_fd(), libc::F_DUPFD, 100) };
    assert_eq!(fd, 100, "the argv names the account file as fd 100");
    // SAFETY: a live descriptor, rewound for bubblewrap to read.
    unsafe { libc::lseek(fd, 0, libc::SEEK_SET) };
    let output = Command::new(bwrap())
        .args(argv)
        .args([
            "--exact",
            "system_view::list_the_visible_filesystem",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    // SAFETY: our copy, no longer needed.
    unsafe { libc::close(fd) };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "sandbox failed: {argv:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing: BTreeMap<String, String> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("VIEW\t"))
        .filter_map(|line| line.split_once('\t'))
        .map(|(path, facts)| (path.to_string(), facts.to_string()))
        .collect();
    assert!(
        listing.contains_key("/usr/lib") && listing.contains_key("/etc/passwd"),
        "the lister saw no system profile: {stdout}"
    );
    listing
}

fn compare(launch: &Launch<'_>) {
    let mut per_path = argv_for(launch);
    per_path.splice(
        0..0,
        ["--ro-bind-data", "100", "/etc/passwd"].map(String::from),
    );
    let mut viewed = per_path.clone();
    launch
        .view
        .compose(&mut viewed)
        .expect("this launch is one the view carries");
    let root = launch
        .view
        .variant_root(launch.variant)
        .display()
        .to_string();
    assert!(
        viewed.iter().any(|arg| arg.starts_with(&root)),
        "expected the {:?} view: {viewed:?}",
        launch.variant
    );
    let binds = |argv: &[String]| argv.iter().filter(|arg| arg.contains("bind")).count();
    println!(
        "{:?}: {} bind operations per path, {} with the view",
        launch.variant,
        binds(&per_path),
        binds(&viewed)
    );

    let before = listing(&per_path);
    let after = listing(&viewed);
    let skeleton = skeleton(launch.variant);
    let mut narrowed = BTreeSet::new();
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for path in paths {
        let (old, new) = (before.get(path), after.get(path));
        if old == new {
            continue;
        }
        let (Some(old), Some(new)) = (old, new) else {
            panic!(
                "{path} is visible only {}: {old:?} / {new:?}",
                if old.is_some() {
                    "per path"
                } else {
                    "in the view"
                }
            );
        };
        // type mode uid ro target
        let old: Vec<&str> = old.split(' ').collect();
        let new: Vec<&str> = new.split(' ').collect();
        let parent_made_read_only = matches!(
            (old.as_slice(), new.as_slice()),
            (["d", _, _, "false", ..], ["d", "755", _, "true", ..])
        );
        assert!(
            skeleton.contains(path.as_str()) && parent_made_read_only,
            "{path} differs beyond a bind parent becoming read-only: per path {old:?}, view {new:?}"
        );
        narrowed.insert(path.clone());
    }
    assert!(
        narrowed.contains("/usr") && narrowed.contains("/etc"),
        "the bind parents were not observed: {narrowed:?}"
    );
}

/// The directories bubblewrap creates as parents of the profile's binds:
/// ancestors of each present entry (and launch file) that no present entry
/// contains.
fn skeleton(variant: Variant) -> BTreeSet<String> {
    let present: Vec<&str> = view_read_paths()
        .filter(|path| Path::new(path).exists())
        .collect();
    let mut dirs = BTreeSet::new();
    for path in present.iter().copied().chain(variant.launch_files()) {
        for ancestor in Path::new(path).ancestors().skip(1) {
            if ancestor == Path::new("/") {
                continue;
            }
            if present.iter().any(|entry| ancestor.starts_with(entry)) {
                continue;
            }
            dirs.insert(ancestor.display().to_string());
        }
    }
    dirs
}

/// A runtime bundle with fonts and this architecture's loader.
fn fake_runtime(dir: &Path) -> PathBuf {
    let runtime = dir.join("runtime");
    std::fs::create_dir_all(runtime.join("lib/fonts")).unwrap();
    let loader = match std::env::consts::ARCH {
        "aarch64" => "lib/lib/ld-linux-aarch64.so.1",
        _ => "lib/lib64/ld-linux-x86-64.so.2",
    };
    std::fs::create_dir_all(runtime.join(loader).parent().unwrap()).unwrap();
    std::fs::write(runtime.join(loader), b"").unwrap();
    runtime
}

/// A regular file inside a profile directory, as a GPU loader manifest is.
fn a_file_inside_the_profile() -> Option<String> {
    [
        "/usr/share/zoneinfo",
        "/usr/share/X11/xkb/rules",
        "/etc/fonts",
    ]
    .iter()
    .filter_map(|dir| std::fs::read_dir(dir).ok())
    .flat_map(Iterator::flatten)
    .map(|entry| entry.path())
    .find(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
    .map(|path| path.display().to_string())
}

fn manifest(net: &str) -> ManifestView {
    ManifestView {
        package_id: "so.tilo.viewtest".to_string(),
        name: None,
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
        kind: "app".to_string(),
        runtime: None,
        optional_runtimes: Vec::new(),
        entrypoint: Entrypoint {
            bin: "bin/app".to_string(),
            r#type: launcher::manifest::EntrypointType::Gui,
            gui_bin: None,
            cli_bin: None,
        },
        sandbox: Sandbox {
            net: Net {
                mode: net.to_string(),
                ..Net::default()
            },
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
            ..Sandbox::default()
        },
        cli_commands: Vec::new(),
        opens_ext: Vec::new(),
        opens_mime: Vec::new(),
        opens_schemes: Vec::new(),
        opens_secondary: false,
        continuity: launcher::manifest::ContinuityContract::default(),
        capabilities: Vec::new(),
    }
}

fn bwrap() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| candidate.is_file())
        .expect("bwrap is how a Tilo app runs; a host without it cannot run this test")
}
