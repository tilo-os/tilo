//! Manifests for the shipped app fleet, checked against the production `pkg`
//! schema, so a schema change that breaks an install breaks this suite first.
//! Every app needs a valid `tilo-app.toml` to be
//! packable, installable, and launchable at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tilo_app_package::id::Kind;
use tilo_app_package::manifest::{
    ContinuityFacet, ContinuitySemanticSchema, EntrypointType, Manifest, NetMode,
};
use tilo_app_package::search_keywords;
use tilo_app_package::validate::validate_manifest;
use tilo_app_package::view::ManifestView;

/// Repo root, derived from this crate's path at `<root>/crates/core/tilo-app-package`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/core/tilo-app-package is three levels below the repo root")
        .to_path_buf()
}

/// The app packages the default collection ships: `dist/apps/app-collections.toml`, which
/// `os/scripts/pack-apps.sh`, `os/image/compile.sh` and the release read. `*` is every native
/// app's manifest under `apps/`; a `tl-<name>` is the tilolang package in `apps/tl/<name>` that
/// replaced the native app it retired. The list is the source, never a count: an app that leaves
/// the tree leaves the collection in the same change, and one the collection names must exist.
fn app_dirs() -> Vec<PathBuf> {
    let apps = repo_root().join("apps");
    let collection = std::fs::read_to_string(repo_root().join("dist/apps/app-collections.toml"))
        .expect("the app collections read");
    let include = collection
        .lines()
        .skip_while(|line| line.trim() != "[collections.default]")
        .skip(1)
        .find_map(|line| line.trim().strip_prefix("include = "))
        .unwrap_or_else(|| panic!("dist/apps/app-collections.toml names no default include"));
    let entries: Vec<&str> = include
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|entry| entry.trim().trim_matches('"'))
        .filter(|entry| !entry.is_empty())
        .collect();
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in entries {
        if entry == "*" {
            let native: Vec<PathBuf> = std::fs::read_dir(&apps)
                .unwrap_or_else(|e| panic!("read {}: {e}", apps.display()))
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.join("tilo-app.toml").is_file())
                .collect();
            assert!(!native.is_empty(), "`*` packs no app under apps/");
            out.extend(native);
            continue;
        }
        let Some(name) = entry.strip_prefix("tl-") else {
            continue;
        };
        let package = apps.join("tl").join(name);
        let source = std::fs::read_to_string(package.join("app.tl"))
            .unwrap_or_else(|e| panic!("{entry} is shipped but {}: {e}", package.join("app.tl").display()));
        assert!(
            source.lines().any(|line| line.starts_with("app so.tilo.")),
            "{entry} names no `so.tilo.` app id"
        );
    }
    out.sort();
    out.dedup();
    out
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

/// Every app with its manifest: one read per app for all the checks.
fn fleet() -> Vec<(PathBuf, Manifest)> {
    app_dirs()
        .into_iter()
        .map(|dir| {
            let manifest = load(&dir);
            (dir, manifest)
        })
        .collect()
}

tilo_test_env::scenario! {
    fn the_shipped_fleets_manifests_validate_and_declare_consistent_metadata_and_policy;

    phase every_shipped_manifest_validates_round_trips_and_has_its_own_id;
    {
        let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (dir, m) in fleet() {
            validate_manifest(&m)
                .unwrap_or_else(|e| panic!("{} fails validation: {e}", dir.display()));
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

    phase shipped_apps_carry_their_names_search_words_and_taglines;
    // What a person types a name to find an app by, and what a row shows: the
    // words live in `search/*.ftl`, joined at pack time, and never in the
    // manifest; every app is named and described in both languages.
    {
        for (app, german) in [
            ("settings", "Einstellungen"),
            ("calendar", "Kalender"),
        ] {
            let view = ManifestView::from_toml_str(
                &std::fs::read_to_string(manifest_path(&repo_root().join("apps").join(app)))
                    .unwrap_or_else(|e| panic!("{app}: {e}")),
            )
            .unwrap_or_else(|e| panic!("{app}: {e}"));
            assert_eq!(
                view.localized_names.get("de").map(String::as_str),
                Some(german),
                "{app}"
            );
        }

        let mut checked = 0;
        for (dir, _) in fleet() {
            let keywords = search_keywords::read(&dir)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            for language in ["en", "de"] {
                assert!(
                    keywords
                        .get(language)
                        .is_some_and(|words| !words.is_empty()),
                    "{} lacks search/{language}.ftl",
                    dir.display()
                );
            }
            let taglines = search_keywords::read_taglines(&dir)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            for language in ["en", "de"] {
                let tagline = taglines
                    .get(language)
                    .unwrap_or_else(|| panic!("{} lacks an {language} app-tagline", dir.display()));
                assert!(
                    tagline.chars().count() <= tilo_app_package::validate::MAX_TAGLINE_CHARS,
                    "{}: {tagline:?} is longer than a row",
                    dir.display()
                );
                assert!(
                    !tagline.contains(" · ") && !tagline.ends_with('.'),
                    "{}: {tagline:?} is one phrase, without separators or a full stop",
                    dir.display()
                );
            }
            // One source: the words are joined at pack time, never committed.
            let source = std::fs::read_to_string(manifest_path(&dir))
                .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path(&dir).display()));
            let view = ManifestView::from_toml_str(&source)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            assert!(
                view.keywords.is_empty(),
                "{} declares [package.keywords]; the words live in search/*.ftl",
                dir.display()
            );
            checked += 1;
        }
        assert_eq!(checked, app_dirs().len(), "every shipped app is checked");
    }

    phase windows_that_run_on_one_device_say_so_in_their_entrypoint;
    // An app says its windows run only on the device that opened them with one word of its
    // entrypoint, and one that says nothing has windows that are views of records. A kind of window
    // no build knows is refused, never taken for a view. Browser is the one shipped app whose
    // window's page lives in its engine, so its device-bound windows and no other app's.
    {
        let activity = repo_root().join("apps/activity");
        assert!(
            !load(&activity).window_runs_on_its_device(),
            "an Activity window is a view of a record"
        );
        let src =
            std::fs::read_to_string(manifest_path(&activity)).expect("Activity's manifest");
        let said = src.replacen("[entrypoint]", "[entrypoint]\nwindow = \"device\"", 1);
        assert_ne!(said, src, "the manifest has an entrypoint to say it in");
        let manifest = Manifest::from_toml_str(&said).expect("the manifest decodes");
        assert!(manifest.window_runs_on_its_device());
        validate_manifest(&manifest).expect("and is a valid manifest");
        let unknown = src.replacen("[entrypoint]", "[entrypoint]\nwindow = \"elsewhere\"", 1);
        assert!(
            Manifest::from_toml_str(&unknown).is_err(),
            "a kind of window no build knows is refused"
        );

        let bound: Vec<String> = fleet()
            .into_iter()
            .map(|(_, manifest)| manifest)
            .filter(Manifest::window_runs_on_its_device)
            .map(|manifest| manifest.package.id)
            .collect();
        // No Rust app's window is bound to its device: the one that was, a page shown by the
        // engine, is a package the language builds, and its manifest says so where it is built
        // (`crates/lang/tilolang-cli`, `packages_showing_a_page_bind_their_window_to_the_device`).
        assert!(bound.is_empty(), "{bound:?}");
    }

    phase shipped_apps_declare_presentation_metadata;
    {
        let semantic_apps = ["so.tilo.browser", "so.tilo.music"];
        let semantic_schemas = BTreeMap::from([
            ("so.tilo.browser", ContinuitySemanticSchema::WebLocationV1),
            ("so.tilo.music", ContinuitySemanticSchema::MediaPositionV1),
            ("so.tilo.chat", ContinuitySemanticSchema::RealtimeSessionV1),
            (
                "so.tilo.code-editor",
                ContinuitySemanticSchema::TextPositionV1,
            ),
        ]);

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

        for (dir, m) in fleet() {
            assert!(
                m.continuity.required.contains(&ContinuityFacet::Presentation),
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

            if !matches!(m.package.kind, Kind::App) {
                continue;
            }
            let name = m.package.name.as_deref().unwrap_or_default();
            assert!(
                !name.trim().is_empty(),
                "{}: no [package].name, so the palette would show a name derived from the id",
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

    phase shipped_manifests_declare_consistent_sandbox_policy;
    {
        let mut services = 0usize;
        for (dir, m) in fleet() {
            let ep = m
                .entrypoint
                .as_ref()
                .unwrap_or_else(|| panic!("{}: no [entrypoint]", dir.display()));
            let bin = ep.bin.to_string_lossy().into_owned();
            assert!(!bin.is_empty(), "{}: empty entrypoint.bin", dir.display());
            assert!(
                !bin.starts_with('/'),
                "{}: entrypoint.bin {bin:?} is absolute; it must be relative to the payload root",
                dir.display()
            );
            assert!(
                bin.starts_with("bin/"),
                "{}: entrypoint.bin {bin:?} is not under the payload's bin/ dir",
                dir.display()
            );

            let has_net_ns = m.sandbox.namespaces.iter().any(|n| n == "net");
            match m.sandbox.net.mode {
                NetMode::Off => assert!(
                    !has_net_ns,
                    "{}: net.mode=off but `net` is in namespaces — the manifest claims isolation it does not apply",
                    dir.display()
                ),
                _ => assert!(
                    has_net_ns,
                    "{}: net.mode is not off but `net` is missing from namespaces",
                    dir.display()
                ),
            }
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

            // `[sandbox.fs]` is retired: apps get the launcher's system read
            // profile and one writable directory, and anything else is a
            // capability the user grants at runtime. Nothing reads the block
            // any more, so a manifest that spells one out would decode and be
            // quietly ignored.
            let path = manifest_path(&dir);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert!(
                !src.contains("[sandbox.fs]"),
                "{}: declares a retired [sandbox.fs] block",
                dir.display()
            );

            if m.package.kind == Kind::Service {
                let draws = matches!(ep.r#type, EntrypointType::Gui | EntrypointType::Both);
                assert_eq!(
                    ep.gui_bin_path().is_some(),
                    draws,
                    "{}: entrypoint type {:?} and the presence of a GUI binary disagree \
                     about whether this service draws",
                    dir.display(),
                    ep.r#type,
                );
                services += 1;
            }
        }
        assert!(
            services >= 1,
            "no service package found to check — this test cannot observe its subject",
        );
    }
}

/// An app's actions reach the person's shell only over the collaboration service: its running
/// window publishes them to the workspace's broker (`CollabRequest::PublishActionSurface`), which lists
/// them in the desktop's catalogue and routes their invocations back. An app whose surface
/// declares verbs but whose sandbox has no `collab` service joins nothing
/// (`CollaborationManager::join_native` disables itself), so none of its actions is ever listed
/// or invocable — not by a palette, an agent or a script.
#[test]
fn an_app_that_publishes_actions_declares_the_service_it_publishes_them_over() {
    let mut checked = 0usize;
    let mut unreachable = Vec::new();
    for (dir, m) in fleet() {
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
