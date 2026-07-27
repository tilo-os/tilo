//! The shipped app fleet's manifests, checked against the real schema.
//!
//! `apps/<name>/qos-app.toml` is what makes an app installable: without
//! one it can never be packed into a `.qpkg`, so it can never be
//! installed, so the shell's launcher can never start it. This suite
//! walks the monorepo's `apps/` directory and holds every manifest to
//! the invariants a launch actually depends on — decode, schema
//! validation, a payload-relative entrypoint, id uniqueness, and the
//! egress/namespace agreement `qos-launcher` reads when it builds the
//! bwrap argv.
//!
//! These assert against the production `pkg` types, not a local copy of
//! the schema, so a schema change that would break a real install breaks
//! this suite first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pkg::id::Kind;
use pkg::manifest::{EntrypointType, Manifest, NetMode};
use pkg::validate::validate_manifest;

/// Repo root, derived from this crate's location
/// (`<root>/crates/packaging/pkg`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/packaging/pkg is three levels below the repo root")
        .to_path_buf()
}

/// Every directory under `apps/` that is a Cargo crate, i.e. every app
/// the workspace actually builds a binary for.
fn app_dirs() -> Vec<PathBuf> {
    let apps = repo_root().join("apps");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&apps)
        .unwrap_or_else(|e| panic!("read {}: {e}", apps.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .collect();
    out.sort();
    assert!(
        out.len() >= 40,
        "expected the full app fleet under apps/, found {}",
        out.len()
    );
    out
}

fn manifest_path(app_dir: &Path) -> PathBuf {
    app_dir.join("qos-app.toml")
}

fn load(app_dir: &Path) -> Manifest {
    let path = manifest_path(app_dir);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Manifest::from_toml_str(&src)
        .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()))
}

/// The gate this whole workstream exists to close: an app crate with no
/// manifest cannot be installed, so it cannot be launched, so it can
/// never appear on the desktop.
#[test]
fn every_app_crate_ships_a_manifest() {
    let missing: Vec<String> = app_dirs()
        .iter()
        .filter(|d| !manifest_path(d).is_file())
        .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        missing.is_empty(),
        "these app crates have no qos-app.toml and therefore cannot be \
         packed, installed or launched: {missing:?}"
    );
}

#[test]
fn every_manifest_passes_schema_validation() {
    for dir in app_dirs() {
        let m = load(&dir);
        validate_manifest(&m)
            .unwrap_or_else(|e| panic!("{} fails validation: {e}", dir.display()));
    }
}

/// `qos-pkg pack` re-serialises the manifest into the payload, and the
/// launcher reads *that* copy — so anything that does not survive a
/// round-trip is silently lost between authoring and launch.
#[test]
fn every_manifest_survives_a_toml_round_trip() {
    for dir in app_dirs() {
        let m = load(&dir);
        let re = m
            .to_toml_string()
            .unwrap_or_else(|e| panic!("{} does not re-encode: {e}", dir.display()));
        let back = Manifest::from_toml_str(&re)
            .unwrap_or_else(|e| panic!("{} does not re-decode: {e}", dir.display()));
        assert_eq!(
            m,
            back,
            "{} changes shape across a pack round-trip",
            dir.display()
        );
    }
}

/// Package ids are the registry's primary key: a collision means
/// installing one app silently replaces another.
#[test]
fn package_ids_are_unique_and_reverse_dns() {
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in app_dirs() {
        let m = load(&dir);
        let id = m.package.id.clone();
        assert!(
            id.starts_with("dev.qos."),
            "{}: id {id:?} is not in the fleet's reverse-DNS namespace",
            dir.display()
        );
        assert!(
            id.len() > "dev.qos.".len(),
            "{}: id {id:?} has an empty trailing component",
            dir.display()
        );
        if let Some(prev) = seen.insert(id.clone(), dir.clone()) {
            panic!(
                "package id {id:?} is claimed by both {} and {}",
                prev.display(),
                dir.display()
            );
        }
    }
}

/// The entrypoint is joined onto the in-sandbox payload mount, so it has
/// to be a payload-relative path — an absolute one would escape the
/// mount and an empty one fails the launch after the fork.
#[test]
fn entrypoints_are_payload_relative() {
    for dir in app_dirs() {
        let m = load(&dir);
        let ep = m
            .entrypoint
            .as_ref()
            .unwrap_or_else(|| panic!("{}: no [entrypoint]", dir.display()));
        let bin = ep.bin.to_string_lossy().into_owned();
        assert!(!bin.is_empty(), "{}: empty entrypoint.bin", dir.display());
        assert!(
            !bin.starts_with('/'),
            "{}: entrypoint.bin {bin:?} is absolute; it must be relative to \
             the payload root",
            dir.display()
        );
        assert!(
            bin.starts_with("bin/"),
            "{}: entrypoint.bin {bin:?} is not under the payload's bin/ dir",
            dir.display()
        );
    }
}

/// `qos-launcher` adds `--share-net` iff `sandbox.net.mode != "off"`,
/// while the `namespaces` list is what an auditor reads. If the two
/// disagree the manifest lies about the app's egress.
#[test]
fn net_mode_and_namespace_agree() {
    for dir in app_dirs() {
        let m = load(&dir);
        let has_net_ns = m.sandbox.namespaces.iter().any(|n| n == "net");
        match m.sandbox.net.mode {
            NetMode::Off => assert!(
                !has_net_ns,
                "{}: net.mode=off but `net` is in namespaces — the manifest \
                 claims isolation it does not apply",
                dir.display()
            ),
            _ => assert!(
                has_net_ns,
                "{}: net.mode is not off but `net` is missing from namespaces",
                dir.display()
            ),
        }
    }
}

/// An allowlist with no hosts is an allowlist of nothing — every request
/// the app makes would be refused, which is a manifest bug, not a
/// policy.
#[test]
fn allowlisted_egress_names_at_least_one_host() {
    for dir in app_dirs() {
        let m = load(&dir);
        if matches!(m.sandbox.net.mode, NetMode::Allowlist) {
            let hosts = m.sandbox.net.allowlist_hosts().unwrap_or(&[]);
            assert!(
                !hosts.is_empty(),
                "{}: net.mode=allowlist with an empty host list",
                dir.display()
            );
            for h in hosts {
                assert!(
                    !h.contains('/') && !h.contains(':'),
                    "{}: allowlist entry {h:?} should be a bare hostname",
                    dir.display()
                );
            }
        }
    }
}

/// A GUI app that never binds the compositor socket into its sandbox has
/// no display and cannot draw — the launch succeeds and the window never
/// appears, which is the worst kind of failure to debug.
#[test]
fn gui_apps_bind_the_compositor_socket() {
    for dir in app_dirs() {
        let m = load(&dir);
        let ep = m.entrypoint.as_ref().expect("[entrypoint]");
        if !matches!(ep.r#type, EntrypointType::Gui | EntrypointType::Both) {
            continue;
        }
        assert!(
            m.sandbox
                .fs
                .read
                .iter()
                .any(|p| p.contains("wayland-0")),
            "{}: GUI entrypoint but no Wayland socket in sandbox.fs.read",
            dir.display()
        );
    }
}

/// The shell's Applications provider hides packages without a GUI
/// entrypoint, so a headless helper must declare itself as one — a
/// service offered as a window is a launch that can never show anything.
#[test]
fn headless_packages_declare_a_cli_entrypoint() {
    let m = load(&repo_root().join("apps/collab-broker"));
    assert_eq!(
        m.package.kind,
        Kind::Service,
        "collab-broker relays CRDT sync over a Unix socket; it is not an app"
    );
    let ep = m.entrypoint.as_ref().expect("[entrypoint]");
    assert_eq!(ep.r#type, EntrypointType::Cli);
    assert!(
        ep.gui_bin_path().is_none(),
        "a headless service must expose no GUI binary to the launcher"
    );
}

/// The human-facing metadata the shell's palette renders. Deriving a
/// name from the id gives `Api Tester` and `Pdf Viewer`; declaring one
/// is why the field exists.
#[test]
fn apps_declare_a_display_name_and_category() {
    const CATEGORIES: &[&str] = &[
        "Accessibility",
        "Development",
        "Multimedia",
        "Network",
        "Office",
        "System",
        "Utility",
    ];
    for dir in app_dirs() {
        let m = load(&dir);
        if !matches!(m.package.kind, Kind::App) {
            continue;
        }
        let name = m.package.name.as_deref().unwrap_or_default();
        assert!(
            !name.trim().is_empty(),
            "{}: no [package].name, so the palette would show a name derived \
             from the id",
            dir.display()
        );
        let cat = m.package.category.as_deref().unwrap_or_default();
        assert!(
            CATEGORIES.contains(&cat),
            "{}: category {cat:?} is not one of {CATEGORIES:?}",
            dir.display()
        );
    }
}

/// Grants are supposed to be the minimum that lets the app work. Two
/// blanket grants would make that claim meaningless, so they are barred
/// outright: `/` and `/etc` wholesale.
#[test]
fn no_manifest_grants_the_whole_filesystem() {
    for dir in app_dirs() {
        let m = load(&dir);
        for pat in m.sandbox.fs.read.iter().chain(m.sandbox.fs.write.iter()) {
            let p = pat.trim_end_matches('/');
            assert!(
                p != "" && p != "/" && p != "/etc" && p != "/usr",
                "{}: blanket grant {pat:?}",
                dir.display()
            );
        }
    }
}
