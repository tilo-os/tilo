//! Manifests for the shipped app fleet, checked against the production `pkg`
//! schema, so a schema change that breaks an install breaks this suite first.
//! Every app needs a valid `tilo-app.toml` to be
//! packable, installable, and launchable at all.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tilo_app_package::id::Kind;
use tilo_app_package::manifest::{
    ContinuityFacet, ContinuitySemanticSchema, EntrypointType, Manifest, NetMode,
};
use tilo_app_package::validate::validate_manifest;

/// Repo root, derived from this crate's path at `<root>/crates/packaging/tilo-app-package`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/packaging/tilo-app-package is three levels below the repo root")
        .to_path_buf()
}

/// Every apps/ directory that's a Cargo crate the workspace builds a binary
/// for, i.e. every app. A crate there with no binary is a library of an app
/// (an `apps/<name>-model` crate of its data, the parts an app loads from
/// its engine library such as `browser-parts`), not an app of its own; nor
/// is a runtime bundle's.
fn app_dirs() -> Vec<PathBuf> {
    let apps = repo_root().join("apps");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&apps)
        .unwrap_or_else(|e| panic!("read {}: {e}", apps.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .filter(|p| builds_a_binary(p))
        .filter(|p| !p.join("tilo-runtime.toml").is_file())
        .collect();
    out.sort();
    assert!(
        out.len() >= 40,
        "expected the full app fleet under apps/, found {}",
        out.len()
    );
    out
}

/// Whether the crate at `dir` builds a binary: a `src/main.rs`, or a
/// `[[bin]]` target of its own.
fn builds_a_binary(dir: &Path) -> bool {
    dir.join("src/main.rs").is_file()
        || std::fs::read_to_string(dir.join("Cargo.toml"))
            .is_ok_and(|manifest| manifest.lines().any(|line| line.trim() == "[[bin]]"))
}

fn manifest_path(app_dir: &Path) -> PathBuf {
    app_dir.join("tilo-app.toml")
}

fn load(app_dir: &Path) -> Manifest {
    let path = manifest_path(app_dir);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Manifest::from_toml_str(&src)
        .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()))
}

#[test]
fn every_shipped_manifest_exists_validates_and_round_trips() {
    {
        let missing: Vec<String> = app_dirs()
            .iter()
            .filter(|d| !manifest_path(d).is_file())
            .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            missing.is_empty(),
            "these app crates have no tilo-app.toml and therefore cannot be \
         packed, installed or launched: {missing:?}"
        );
    }
    {
        for dir in app_dirs() {
            let m = load(&dir);
            validate_manifest(&m)
                .unwrap_or_else(|e| panic!("{} fails validation: {e}", dir.display()));
        }
    }
    {
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
    {
        for dir in app_dirs() {
            let m = load(&dir);
            validate_manifest(&m)
                .unwrap_or_else(|e| panic!("{}: refused by its own installer: {e}", dir.display()));
        }
    }
    {
        let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
        for dir in app_dirs() {
            let m = load(&dir);
            let id = m.package.id.clone();
            assert!(
                id.starts_with("so.tilo."),
                "{}: id {id:?} is not in the fleet's reverse-DNS namespace",
                dir.display()
            );
            assert!(
                id.len() > "so.tilo.".len(),
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
}

#[test]
fn shipped_apps_declare_presentation_metadata() {
    {
        let semantic_apps = ["so.tilo.browser", "so.tilo.ebook", "so.tilo.music"];
        let semantic_schemas = BTreeMap::from([
            ("so.tilo.browser", ContinuitySemanticSchema::WebLocationV1),
            (
                "so.tilo.ebook",
                ContinuitySemanticSchema::PublicationPositionV1,
            ),
            ("so.tilo.music", ContinuitySemanticSchema::MediaPositionV1),
            ("so.tilo.chat", ContinuitySemanticSchema::RealtimeSessionV1),
            (
                "so.tilo.code-editor",
                ContinuitySemanticSchema::TextPositionV1,
            ),
        ]);

        for dir in app_dirs() {
            let m = load(&dir);
            assert!(
                m.continuity
                    .required
                    .contains(&ContinuityFacet::Presentation),
                "{}: app handoff must retain framework presentation state",
                dir.display()
            );
            assert_eq!(
                m.continuity.required.contains(&ContinuityFacet::Semantic),
                semantic_apps.contains(&m.package.id.as_str()),
                "{}: signed semantic-continuity declaration drifted from the apps that implement it",
                dir.display()
            );
            assert_eq!(
                m.continuity.semantic_schema,
                semantic_schemas.get(m.package.id.as_str()).copied(),
                "{}: portable semantic schema drifted from runtime validation",
                dir.display()
            );
        }
    }
    {
        // Freedesktop main categories, narrowed to the ones Tilo actually groups
        // by. Adding one is deliberate: the palette groups on this string, so a
        // typo would silently create a category of one.
        const CATEGORIES: &[&str] = &[
            "Accessibility",
            "Development",
            "Game",
            "Graphics",
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn shipped_manifests_declare_consistent_sandbox_policy() {
    {
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
    {
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
    {
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
    {
        let mut checked = 0usize;
        for dir in app_dirs() {
            let m = load(&dir);
            assert!(
                m.sandbox.fs.read.is_empty() && m.sandbox.fs.write.is_empty(),
                "{}: declares sandbox.fs paths (read={:?} write={:?}). An app does \
             not choose what it reads or where it writes — reads come from the \
             launcher's system profile, writes go to the one directory the \
             launcher gives it, and anything else is a capability the user \
             grants at runtime.",
                dir.display(),
                m.sandbox.fs.read,
                m.sandbox.fs.write,
            );
            checked += 1;
        }
        assert!(checked >= 40, "expected the shipped fleet, found {checked}");
    }
    {
        let mut checked = 0usize;
        for dir in app_dirs() {
            let m = load(&dir);
            if m.package.kind != Kind::Service {
                continue;
            }
            let ep = m
                .entrypoint
                .as_ref()
                .unwrap_or_else(|| panic!("{}: service with no [entrypoint]", dir.display()));
            let draws = matches!(ep.r#type, EntrypointType::Gui | EntrypointType::Both);
            assert_eq!(
                ep.gui_bin_path().is_some(),
                draws,
                "{}: entrypoint type {:?} and the presence of a GUI binary disagree \
             about whether this service draws",
                dir.display(),
                ep.r#type,
            );
            checked += 1;
        }
        assert!(
            checked >= 1,
            "no service package found to check — this test cannot observe its subject",
        );
    }
}

/// An app's actions reach the person's desk only over the collaboration service: its running
/// window publishes them to the workspace's broker (`CollabRequest::PublishActionSurface`), which lists
/// them in the desktop's catalogue and routes their invocations back. An app whose surface
/// declares verbs but whose sandbox has no `collab` service joins nothing
/// (`CollaborationManager::join_native` disables itself), so none of its actions is ever listed
/// or invocable — not by a palette, an agent or a script.
#[test]
fn an_app_that_publishes_actions_declares_the_service_it_publishes_them_over() {
    let mut checked = 0usize;
    let mut unreachable = Vec::new();
    for dir in app_dirs() {
        let m = load(&dir);
        if m.package.kind != Kind::App {
            continue;
        }
        let surface = std::fs::read_to_string(dir.join("surface.toml")).unwrap_or_default();
        if !surface
            .lines()
            .any(|line| line.trim() == "[[resources.verbs]]")
        {
            continue;
        }
        checked += 1;
        if !m.sandbox.services.contains_key("collab") {
            unreachable.push(dir.display().to_string());
        }
    }
    assert!(
        checked >= 20,
        "expected the fleet's acting apps, found {checked}"
    );
    assert!(
        unreachable.is_empty(),
        "these apps declare actions in surface.toml but no `collab` service in \
         [sandbox.services], so a running window of theirs publishes none: {unreachable:?}"
    );
}
