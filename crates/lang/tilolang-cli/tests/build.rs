//! `tilolang build`: every package of a build is a signed Tilo app of its own.

use std::path::Path;

use tilo_app_package::manifest::{Manifest, Platform};
use tilolang_cli::package::{self, BuildOptions};

use crate::support::{copy_package, packages, with_used};

#[test]
fn each_package_is_a_signed_app_that_runs_on_the_tilolang_runtime() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["today"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir: source.join("today"),
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let ids: Vec<&str> = apps.iter().map(|app| app.app.as_str()).collect();
    assert_eq!(
        ids.last(),
        Some(&"so.tilo.lang.today"),
        "the package last, after what it uses"
    );
    for used in [
        "so.tilo.notes",
        "so.tilo.tasks",
        "so.tilo.chat",
        "so.tilo.lang.discussion",
    ] {
        assert!(ids.contains(&used), "{used} is an app of its own: {ids:?}");
    }
    for app in &apps {
        assert!(app.tapp.is_file(), "{} has its .tapp", app.app);
        let manifest =
            Manifest::from_toml_str(&std::fs::read_to_string(&app.manifest).unwrap()).unwrap();
        assert_eq!(manifest.package.id, app.app, "the app id is the package's");
        let entrypoint = manifest.entrypoint.as_ref().unwrap();
        assert_eq!(
            entrypoint.runtime.as_deref(),
            Some(package::default_tilolang_runtime().as_str()),
            "it runs on the tilolang runtime bundle"
        );
        assert_eq!(entrypoint.bin, Path::new(package::RUNTIME_BIN));
        // It runs where the runtime bundle runs: a device the runtime is not built for is told so
        // rather than failing to start it.
        assert_eq!(
            manifest.package.platforms(),
            [Platform::Linux, Platform::Macos, Platform::Web],
            "{} names the platforms its runtime runs on",
            app.app
        );
        assert!(
            manifest.package.platform_reason.is_none(),
            "a list that narrows nothing gives no reason"
        );
        assert_eq!(manifest.package.icon.as_deref(), Some("icon.svg"));
        assert!(app.payload.join("icon.svg").is_file());
        assert!(app.payload.join("app.wasm").is_file());
        // Collaboration and the filesystem at the version the runtime serves them, or the install
        // is refused.
        for service in ["collab", "tilo-fs"] {
            let requirement = manifest
                .sandbox
                .services
                .get(service)
                .unwrap_or_else(|| panic!("{} asks for no {service}", app.app));
            assert_eq!(
                requirement.version,
                runtime_portal::RUNTIME_INTERFACE_VERSION,
                "{service} at the version the runtime serves"
            );
        }
    }
    let today = apps.last().unwrap();
    let notes = apps.iter().find(|app| app.app == "so.tilo.notes").unwrap();
    let today_manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&today.manifest).unwrap()).unwrap();
    assert_eq!(today_manifest.package.name.as_deref(), Some("Today"));
    assert_eq!(today_manifest.package.category.as_deref(), Some("Office"));
    // What the app is called where it is not English: its catalogue's own word for its name, so
    // every surface that names it — a launcher, a search row, a change it made — reads "Heute".
    assert_eq!(
        today_manifest
            .package
            .names
            .get("de-DE")
            .map(String::as_str),
        Some("Heute"),
    );
    assert!(
        !today_manifest.package.names.contains_key("en-US"),
        "the name its declaration gives needs no table of its own"
    );
    let installed = today_reads_notes(today, notes);
    declared_actions(&apps, &today_manifest);
    no_payload_carries_the_standard_library(&apps, &installed);
}

/// The kinds of file an app's actions open (`opens:`) are what its manifest offers, so Files and
/// the person's own file system open exactly those with the app, and no name of the app stands in
/// a table of file types anywhere.
#[test]
fn a_package_offers_the_files_its_actions_open() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["notes"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir: source.join("notes"),
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let notes = apps.iter().find(|app| app.app == "so.tilo.notes").unwrap();
    let manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&notes.manifest).unwrap()).unwrap();
    assert_eq!(
        manifest.providers.mime,
        [
            "text/markdown",
            "text/plain",
            "application/zip",
            "application/json"
        ],
        "the media types its importing actions declare"
    );
    assert_eq!(
        manifest.providers.ext,
        ["md", "markdown", "txt", "zip", "json"],
        "the extensions they declare"
    );
    // A package whose actions open nothing offers nothing.
    let discussion = apps
        .iter()
        .find(|app| app.app == "so.tilo.lang.discussion")
        .expect("the package Notes is made with is built");
    let discussion =
        Manifest::from_toml_str(&std::fs::read_to_string(&discussion.manifest).unwrap()).unwrap();
    assert!(
        discussion.providers.mime.is_empty() && discussion.providers.ext.is_empty(),
        "an app that opens no files offers none: {:?}",
        discussion.providers
    );
}

/// Today uses Notes: it declares that it reads their kinds, carries their module and their schema.
fn today_reads_notes(
    today: &package::BuiltApp,
    notes: &package::BuiltApp,
) -> tilolang_tilo::bundle::Bundle {
    let surface = collab_types_surface(&std::fs::read(today.payload.join("surface.toml")).unwrap());
    let notes_surface =
        collab_types_surface(&std::fs::read(notes.payload.join("surface.toml")).unwrap());
    assert!(
        surface.reads_any_of(&notes_surface),
        "Today's surface declares that it reads Notes' kinds"
    );
    assert!(today.payload.join("imports/so.tilo.notes.wasm").is_file());
    let installed = tilolang_tilo::bundle::Bundle::read(&today.payload).unwrap();
    assert_eq!(installed.meta.app, "so.tilo.lang.today");
    assert!(
        installed
            .imports
            .iter()
            .any(|import| import.meta.app == "so.tilo.notes")
    );
    installed
}

/// The action API each package's manifest declares: Today's morning cleanup calls Tasks' and
/// Notes' actions, so Today declares that it invokes another app's actions, which the broker reads
/// as the ceiling on what it may ask of their action APIs; a package that calls only its own actions
/// declares nothing beyond them.
fn declared_actions(apps: &[package::BuiltApp], today: &Manifest) {
    let declared = |app: &package::BuiltApp| {
        Manifest::from_toml_str(&std::fs::read_to_string(&app.manifest).unwrap())
            .unwrap()
            .actions
    };
    assert!(
        today.actions.invoke,
        "Today declares it invokes another app's actions: {:?}",
        today.actions
    );
    assert!(
        !today.actions.query,
        "it reads no catalogue of the person's actions"
    );
    let discussion = apps
        .iter()
        .find(|app| app.app == "so.tilo.lang.discussion")
        .expect("the package Tasks and Notes are made with is built");
    let discussion = declared(discussion);
    assert!(
        !discussion.invoke && !discussion.query,
        "Discussion calls only its own actions: {discussion:?}"
    );
}

/// Every app compiles with the standard library and carries none of it: it records the library it
/// was built with, and names the runtime keyed by that library, which brings it. Its views are the
/// app's, expanded where it calls them, and its functions are code the runtime's copy carries.
fn no_payload_carries_the_standard_library(
    apps: &[package::BuiltApp],
    installed: &tilolang_tilo::bundle::Bundle,
) {
    let runtime = tilolang_tilo::runtime_of(tilolang::stdlib::digest());
    for app in apps {
        assert!(
            !app.payload.join("imports/tilo.std.wasm").exists(),
            "{} leaves the standard library to its runtime",
            app.app
        );
        let manifest =
            Manifest::from_toml_str(&std::fs::read_to_string(&app.manifest).unwrap()).unwrap();
        assert_eq!(
            manifest
                .entrypoint
                .as_ref()
                .and_then(|entrypoint| entrypoint.runtime.as_deref()),
            Some(runtime.as_str()),
            "{} runs on the runtime of the library it was built with",
            app.app
        );
    }
    assert_eq!(installed.meta.std, tilolang::stdlib::digest());
    assert!(
        installed
            .imports
            .iter()
            .all(|import| import.meta.app != tilolang::stdlib::APP),
        "Today carries no library"
    );
}

/// A package that reads the platform's published resources (the shell's quick settings read the
/// device's) builds: the platform's apps are described from their surfaces, not looked for among
/// the packages.
#[test]
fn the_platforms_a_package_names_are_the_runtimes() {
    // One runtime bundle runs every tilolang app, so where it runs is where they run: the
    // manifest each package gets names the bundle's platforms, never the default.
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tilolang-process/tilo-runtime.toml"),
    )
    .unwrap();
    let runtime = Manifest::from_toml_str(&text).unwrap();
    let named: Vec<String> = runtime
        .package
        .platforms()
        .iter()
        .map(|platform| format!("\"{}\"", platform.as_str()))
        .collect();
    assert_eq!(format!("[{}]", named.join(", ")), package::PLATFORMS);
}

#[test]
fn a_package_that_uses_the_platforms_apps_builds() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["shell"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir: source.join("shell"),
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let shell = apps.last().unwrap();
    assert_eq!(shell.app, "so.tilo.shell");
    assert!(shell.tapp.is_file());
    assert!(
        apps.iter()
            .all(|app| app.app != tilolang_tilo::platform_apps::DEVICE_APP),
        "the device is the platform's, not built here: {apps:?}"
    );
}

fn collab_types_surface(bytes: &[u8]) -> collab_types::surface::SurfaceFile {
    collab_types::surface::SurfaceFile::parse(bytes).unwrap()
}

#[cfg(target_os = "linux")]
fn setenv<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
    argv.windows(3)
        .find(|window| window[0] == "--setenv" && window[1] == key)
        .map(|window| window[2].as_str())
}

/// The launcher runs a built app as the tilolang runtime's binary over the app's payload, under the
/// app's own id, with its icon and its surface.
#[test]
#[cfg(target_os = "linux")]
fn the_launcher_runs_a_built_app_on_the_runtime_binary_over_its_payload() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("tl/counter");
    copy_package(&packages().join("counter"), &dir);
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: "tilolang/0.1.0+tilolang".into(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let [counter] = apps.as_slice() else {
        panic!("one app: {apps:?}");
    };
    let manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&counter.manifest).unwrap()).unwrap();
    let view = launcher::manifest::ManifestView::try_from(manifest).unwrap();
    let argv = launcher::sandbox::build_bwrap_argv(
        &view,
        &counter.payload,
        None,
        &[],
        None,
        &launcher::capabilities::CapabilityPlan::empty(),
    );
    assert_eq!(
        argv.last().map(String::as_str),
        Some("/run/tilo-runtime/0.1.0+tilolang/bin/tilolang-process"),
        "the runtime bundle's binary runs"
    );
    let payload = launcher::sandbox::payload_mount_for("so.tilo.lang.counter");
    assert_eq!(
        setenv(&argv, "TILO_APP_MODULE").map(Path::new),
        Some(payload.as_path()),
        "pointed at the app's payload"
    );
    assert_eq!(
        setenv(&argv, "TILO_PACKAGE_ID"),
        Some("so.tilo.lang.counter")
    );
    assert_eq!(
        setenv(&argv, "TILO_APP_ICON").map(Path::new),
        Some(payload.join("icon.svg").as_path()),
        "its signed icon"
    );
    assert!(
        setenv(&argv, "TILO_SURFACE").is_some(),
        "its surface: {argv:?}"
    );
}

/// The app header's `splash_budget` is the manifest's `[entrypoint] splash-budget`, and a header
/// that declares none leaves the manifest without one rather than inventing a promise.
#[test]
fn the_splash_budget_a_header_declares_is_the_manifests_entrypoint_budget() {
    let scratch = tempfile::tempdir().unwrap();
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let build = |dir: &Path| {
        let options = BuildOptions {
            dir: dir.to_path_buf(),
            search: Vec::new(),
            out: scratch.path().join("out"),
            tilo_runtime: "tilo-runtime/2026.07".into(),
            tilolang_runtime: package::default_tilolang_runtime(),
            version: "0.1.0".into(),
            locked: false,
        };
        let apps = package::build(&options, &identity).unwrap();
        apps.last().expect("the app").manifest.clone()
    };
    let declared = scratch.path().join("tl/declared");
    std::fs::create_dir_all(&declared).unwrap();
    std::fs::write(
        declared.join("app.tl"),
        "app t.budget \"Budget\" (category: .utility, splash_budget: 250)\n\ntype Note {\n    title: Text\n}\n\nroot note: Note\n\nview note_view(n: Note) {\n    std.page(n) {\n        ui.Text(\"{n.title}\", role: .title)\n    }\n}\n\ndefault view note_view for Note\n",
    )
    .unwrap();
    let manifest = build(&declared);
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("splash-budget = 250"), "{text}");
    let parsed = Manifest::from_toml_str(&text).unwrap();
    assert_eq!(
        parsed.entrypoint.expect("an entrypoint").splash_budget_ms,
        250
    );

    let plain = scratch.path().join("tl/plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(
        plain.join("app.tl"),
        "app t.plain \"Plain\" (category: .utility)\n\ntype Note {\n    title: Text\n}\n\nroot note: Note\n\nview note_view(n: Note) {\n    std.page(n) {\n        ui.Text(\"{n.title}\", role: .title)\n    }\n}\n\ndefault view note_view for Note\n",
    )
    .unwrap();
    let manifest = build(&plain);
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(
        !text.contains("splash-budget"),
        "a header that promises no budget leaves the manifest's default: {text}"
    );
}

/// Write a Fluent comment large enough to push a tilolang package's `.tapp`
/// archive past [`tilo_app_package::MAX_APP_TAPP_BYTES`] without touching its
/// manifest or breaking compilation.
fn bloat_locale_past_app_limit(package_dir: &Path) {
    let locale_dir = package_dir.join("locales");
    std::fs::create_dir_all(&locale_dir).unwrap();
    let line = format!("# {}\n", "a".repeat(1020));
    let lines = usize::try_from(tilo_app_package::MAX_APP_TAPP_BYTES)
        .unwrap()
        .div_ceil(line.len())
        .saturating_add(1);
    std::fs::write(locale_dir.join("zz-ZZ.ftl"), line.repeat(lines)).unwrap();
}

fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn run_tilolang_build(dir: &Path, out: &Path, identity: &Path) -> std::process::Output {
    use tilo_test_env::SpawnUnderEnvLock as _;
    std::process::Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .args(["build"])
        .arg(dir)
        .arg("--out")
        .arg(out)
        .arg("--identity")
        .arg(identity)
        .output_locked()
        .expect("run tilolang build")
}

#[test]
fn an_oversized_tilolang_app_is_refused_and_leaves_nothing_behind() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl/counter");
    copy_package(&packages().join("counter"), &source);
    let identity_path = scratch.path().join("publisher.json");
    let _ = tilolang_cli::identity_at(&identity_path).unwrap();

    // First build a valid package into `prior_out` so we can confirm a later
    // failing build leaves an existing output untouched.
    let prior_out = scratch.path().join("prior-out");
    let ok = run_tilolang_build(&source, &prior_out, &identity_path);
    assert!(
        ok.status.success(),
        "valid build failed: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let prior_tapp = std::fs::read(prior_out.join("counter.tapp")).unwrap();
    let prior_manifest = std::fs::read(prior_out.join("counter/manifest.toml")).unwrap();
    let prior_entries = dir_entries(&prior_out);

    // Push the app's payload past its manifest's size limit.
    bloat_locale_past_app_limit(&source);

    // 1. Fresh non-existent output directory: refused and not created.
    let fresh_out = scratch.path().join("fresh-out");
    let failed = run_tilolang_build(&source, &fresh_out, &identity_path);
    assert!(
        !failed.status.success(),
        "oversized app must be refused: stdout={}",
        String::from_utf8_lossy(&failed.stdout)
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains("limit") && stderr.contains("bytes"),
        "expected size limit error on stderr, got: {stderr}"
    );
    assert!(
        !fresh_out.exists(),
        "failed build must not leave behind {}",
        fresh_out.display()
    );

    // 2. Existing empty output directory: refused and still completely empty.
    let empty_out = scratch.path().join("empty-out");
    std::fs::create_dir(&empty_out).unwrap();
    let failed_empty = run_tilolang_build(&source, &empty_out, &identity_path);
    assert!(!failed_empty.status.success());
    assert!(
        dir_entries(&empty_out).is_empty(),
        "failed build left files behind: {:?}",
        dir_entries(&empty_out)
    );

    // 3. Existing output directory with a prior valid build: untouched and clean.
    let failed_prior = run_tilolang_build(&source, &prior_out, &identity_path);
    assert!(!failed_prior.status.success());
    assert_eq!(dir_entries(&prior_out), prior_entries);
    assert_eq!(
        std::fs::read(prior_out.join("counter.tapp")).unwrap(),
        prior_tapp,
        "prior valid .tapp must stay untouched"
    );
    assert_eq!(
        std::fs::read(prior_out.join("counter/manifest.toml")).unwrap(),
        prior_manifest,
        "prior valid manifest must stay untouched"
    );
}

#[test]
fn a_multi_package_build_that_fails_leaves_nothing_in_the_output_directory() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["notes"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    // `notes` uses `discussion`, so `discussion` is delivered before `notes`.
    // Bloating `notes` must not leave `discussion` behind in `out`.
    bloat_locale_past_app_limit(&source.join("notes"));
    let identity_path = scratch.path().join("publisher.json");
    let _ = tilolang_cli::identity_at(&identity_path).unwrap();

    let out = scratch.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let failed = run_tilolang_build(&source.join("notes"), &out, &identity_path);
    assert!(
        !failed.status.success(),
        "oversized multi-package build must fail"
    );
    assert!(
        dir_entries(&out).is_empty(),
        "failed multi-package build left files behind: {:?}",
        dir_entries(&out)
    );
}

#[test]
fn a_valid_tilolang_package_has_verified_exact_archive_bytes() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl/counter");
    copy_package(&packages().join("counter"), &source);
    let identity = tilo_app_package::SigningIdentity::from_seed(&[0x22; 32]);
    let options = BuildOptions {
        dir: source,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: "tilolang/0.1.0+tilolang".into(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let [counter] = apps.as_slice() else {
        panic!("one app: {apps:?}");
    };
    let on_disk = std::fs::read(&counter.tapp).unwrap();
    let manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&counter.manifest).unwrap()).unwrap();
    let repacked = tilo_app_package::pack::pack(&counter.payload, &manifest, &identity).unwrap();
    let expected = tilo_app_package::tapp::to_bytes(&repacked.meta, Some(&repacked)).unwrap();
    assert_eq!(
        on_disk, expected,
        "built .tapp bytes must match the canonical archive encoding"
    );
    let decoded =
        tilo_app_package::tapp::read(on_disk.as_slice(), &mut tilo_app_package::DiscardBlobs)
            .unwrap();
    assert!(decoded.is_complete());
    assert!(matches!(
        tilo_app_package::verify(&decoded.meta, &[identity.public_key_bytes()]),
        tilo_app_package::Verification::Trusted { .. }
    ));

    // A failure during staging (such as an unknown icon) leaves nothing behind
    // in the output directory.
    let bad_source = scratch.path().join("tl/bad-icon");
    copy_package(&packages().join("counter"), &bad_source);
    let app_tl = bad_source.join("app.tl");
    let text = std::fs::read_to_string(&app_tl).unwrap();
    std::fs::write(
        &app_tl,
        text.replace("icon: .hash", "icon: .not_a_real_platform_icon"),
    )
    .unwrap();
    let bad_out = scratch.path().join("bad-out");
    std::fs::create_dir(&bad_out).unwrap();
    let bad_options = BuildOptions {
        dir: bad_source,
        out: bad_out.clone(),
        ..options
    };
    assert!(package::build(&bad_options, &identity).is_err());
    assert!(
        dir_entries(&bad_out).is_empty(),
        "staging failure left files behind: {:?}",
        dir_entries(&bad_out)
    );
}

/// An app that opens links of a URI scheme declares it, as a native app's manifest claims one:
/// the video app claims `tube`, so the session opens a `tube:` link in it.
#[test]
fn an_app_that_claims_a_uri_scheme_declares_it_in_its_manifest() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["tube"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir: source.join("tube"),
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let tube = apps
        .iter()
        .find(|app| app.app == "so.tilo.tube")
        .expect("the package declares an app");
    let text = std::fs::read_to_string(&tube.manifest).unwrap();
    let manifest = Manifest::from_toml_str(&text).unwrap();
    assert_eq!(
        manifest.providers.schemes,
        ["tube"],
        "the scheme its `app` declaration claims"
    );
    // What the session reads to decide which app opens a link of a scheme.
    let view = tilo_app_package::view::ManifestView::from_toml_str(&text).unwrap();
    assert_eq!(view.opens_schemes, ["tube"]);
    // A link reaches the app through the `open-link` its manifest offers, on the resource it
    // names, whether or not a window of it is open.
    let open_link = manifest
        .offers
        .iter()
        .find(|offer| offer.action == "open-link")
        .expect("the app offers the link it opens");
    assert_eq!(open_link.resource, "tl:so.tilo.tube:root-thwdswkk");
    assert_eq!(open_link.resource_kind(), "tl.so.tilo.tube.YouTube");
    assert_eq!(
        open_link
            .asked()
            .iter()
            .map(|asked| asked.name.as_str())
            .collect::<Vec<_>>(),
        ["link"],
        "the parameter the link is handed in"
    );
    assert_eq!(
        open_link.operation,
        tilo_app_package::manifest::OfferOperation::View
    );
}

/// A check builds what is committed: `--locked` refuses a package whose ids lock its sources have
/// moved past, says how to write it, and leaves the lock and the output as they were. Without it,
/// a build writes the lock, as a person changing the sources wants.
#[test]
fn a_locked_build_refuses_an_out_of_date_ids_lock_and_writes_nothing() {
    use tilo_test_env::SpawnUnderEnvLock as _;
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl/counter");
    copy_package(&packages().join("counter"), &source);
    let lock = source.join("app.ids");
    let stale: String = std::fs::read_to_string(&lock)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("counter = "))
        .map(|line| line.to_owned() + "\n")
        .collect();
    std::fs::write(&lock, &stale).unwrap();
    let out = scratch.path().join("out");
    let identity = scratch.path().join("publisher.json");
    let build = |locked: bool| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tilolang"));
        command.arg("build").arg(&source).arg("--out").arg(&out);
        command.arg("--identity").arg(&identity);
        if locked {
            command.arg("--locked");
        }
        command.output_locked().expect("run tilolang build")
    };

    let refused = build(true);
    assert!(
        !refused.status.success(),
        "a locked build of a stale lock fails"
    );
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        said.contains("the ids lock (app.ids)")
            && said.contains("out of date")
            && said.contains("tilolang check --write-ids"),
        "it says what is out of date and how to write it: {said}"
    );
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        stale,
        "the lock is untouched"
    );
    assert!(!out.join("counter.tapp").exists(), "nothing is built");

    let unlocked = build(false);
    assert!(
        unlocked.status.success(),
        "{}",
        String::from_utf8_lossy(&unlocked.stderr)
    );
    assert!(
        std::fs::read_to_string(&lock)
            .unwrap()
            .contains("\ncounter = "),
        "an unlocked build writes the lock"
    );
    assert!(
        build(true).status.success(),
        "and then a locked build passes"
    );
}

/// `dir` as a package that carries a program: bun runs its `main.js` as the app's held
/// connection, over a module of its own and a dependency it vendors.
fn a_program_package(dir: &Path) {
    std::fs::write(
        dir.join("app.toml"),
        r#"[package]
optional-runtimes = ["bun/1.4.5+bun"]

[entrypoint]
type = "cli"
runtime = "bun/1.4.5+bun"
bin = "bin/bun"
script = "main.js"
connection = true

[capabilities]
required = ["network.http", "secrets"]

[sandbox.services]
native = "required@40"

[sandbox.net]
mode = "allowlist"
hosts = ["gateway.example", "*.example.media"]
"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.js"), "export function run() {}\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/tilo.js"), "export const calls = [];\n").unwrap();
    std::fs::create_dir_all(dir.join("node_modules/tilo-sdk-native")).unwrap();
    std::fs::write(
        dir.join("node_modules/tilo-sdk-native/index.js"),
        "module\n",
    )
    .unwrap();
    std::fs::write(dir.join("node_modules/vendored.js"), "export {}\n").unwrap();
}

/// A package that carries a program (`app.toml`): its payload holds the program's files beside the
/// module, and its manifest's entrypoint runs the program with its runtime's interpreter, as the
/// app's held connection, while the tilolang runtime the app's actions and views run on still
/// installs with it. The SDK's native addon is the runtime package's, never a package's own copy.
#[test]
fn a_package_that_carries_a_program_ships_it_beside_its_module() {
    use tilo_app_package::manifest::{EntrypointType, NetMode};

    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    let dir = source.join("counter");
    copy_package(&packages().join("counter"), &dir);
    a_program_package(&dir);

    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let built = apps.last().expect("the package is built");
    let manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&built.manifest).unwrap()).unwrap();
    let entrypoint = manifest.entrypoint.as_ref().expect("an entrypoint");
    assert_eq!(entrypoint.r#type, EntrypointType::Cli);
    assert_eq!(entrypoint.runtime.as_deref(), Some("bun/1.4.5+bun"));
    assert_eq!(entrypoint.bin, Path::new("bin/bun"));
    assert_eq!(
        entrypoint.script.as_deref(),
        Some(Path::new("main.js")),
        "the runtime's binary runs the package's script"
    );
    assert!(
        entrypoint.connection,
        "the program is the app's held connection"
    );
    for runtime in [
        "bun/1.4.5+bun",
        package::default_tilolang_runtime().as_str(),
    ] {
        assert!(
            manifest
                .package
                .required_runtimes
                .iter()
                .any(|declared| declared == runtime),
            "{runtime} installs with the app: {:?}",
            manifest.package.required_runtimes
        );
    }
    assert_eq!(manifest.sandbox.net.mode, NetMode::Allowlist);
    assert_eq!(
        manifest.sandbox.net.hosts,
        ["gateway.example", "*.example.media"],
        "the hosts the program reaches, as it declared them"
    );
    assert!(
        manifest.sandbox.namespaces.iter().any(|name| name == "net"),
        "a program that reaches a host opens a socket: {:?}",
        manifest.sandbox.namespaces
    );
    assert!(
        manifest
            .capabilities
            .required
            .iter()
            .any(|name| name == "secrets")
    );
    assert!(manifest.sandbox.services.contains_key("native"));

    for file in [
        "app.wasm",
        "app.ids",
        "surface.toml",
        "main.js",
        "src/tilo.js",
    ] {
        assert!(
            built.payload.join(file).is_file(),
            "the payload holds {file}"
        );
    }
    assert!(
        built.payload.join("node_modules/vendored.js").is_file(),
        "a program ships the modules it vendors"
    );
    assert!(
        !built.payload.join("node_modules/tilo-sdk-native").exists(),
        "the SDK's native addon is the runtime package's, not a package's copy"
    );
    assert!(
        !built.payload.join("app.tl").exists(),
        "sources are compiled, not shipped"
    );
    assert!(!built.payload.join("app.toml").exists());
}

/// The Discord bridge is such a package: its `app.toml` says its program is `main.js` on bun, and
/// the build of `apps/tl/discord` runs that program as the app, with the module its records and
/// actions are beside it.
#[test]
fn the_discord_bridge_packages_its_program_as_the_entrypoint() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["discord"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir: source.join("discord"),
        search: vec![source.clone(), packages()],
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let apps = package::build(&options, &identity).unwrap();
    let built = apps.last().expect("the package is built");
    assert_eq!(built.app, "so.tilo.discord.preview");
    let manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&built.manifest).unwrap()).unwrap();
    let entrypoint = manifest.entrypoint.as_ref().expect("an entrypoint");
    assert_eq!(entrypoint.script.as_deref(), Some(Path::new("main.js")));
    assert_eq!(entrypoint.runtime.as_deref(), Some("bun/1.4.5+bun"));
    assert!(entrypoint.connection);
    assert!(built.payload.join("main.js").is_file());
    assert!(built.payload.join("src/plan.js").is_file());
    assert!(built.payload.join("app.wasm").is_file());
}
