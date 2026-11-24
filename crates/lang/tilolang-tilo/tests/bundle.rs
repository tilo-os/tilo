//! An installed bundle runs the packages it uses: their types resolve, their views draw and
//! their actions run on the resources of theirs it holds, as Today draws the todo list's rows.

use crate::fixtures::Workspace;
use tilolang_runtime::{Actor, UiNode, Value};
use tilolang_tilo::bundle::Bundle;

/// The package in `dir` compiled with the packages it uses, as `tilolang build` bundles it: its
/// imports resolve as `check` and `build` resolve them, against what the platform and the native
/// apps beside it publish ([`tilolang_tilo::catalogue::published_packages`]) and then the
/// packages it uses, which the bundle carries.
fn bundle_of(dir: &std::path::Path) -> Bundle {
    let published =
        tilolang_tilo::catalogue::published_packages(&tilolang::source::search_of(dir, &[]))
            .unwrap();
    let built = tilolang::source::compile_package(
        dir,
        &[],
        &published,
        &mut tilolang::OsEntropy::default(),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    let (last, earlier) = built.split_last().unwrap();
    let used = &last.compiled.meta.uses;
    Bundle::new(
        &last.compiled,
        earlier
            .iter()
            .map(|earlier| &earlier.compiled)
            .filter(|earlier| used.contains(&earlier.meta.app)),
    )
}

/// Today compiled with the packages it uses.
fn today() -> Bundle {
    bundle_of(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/today"))
}

/// Every text `node` and the nodes inside it show.
fn texts_of(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The keys of the lists under `node`.
fn lists(node: &UiNode) -> Vec<String> {
    node.of_kind("list")
        .into_iter()
        .map(|list| list.key.clone())
        .collect()
}

#[test]
fn a_bundle_draws_the_rows_of_the_packages_it_uses() {
    let (replica, _) = crate::fixtures::Replica::new("ada", Workspace::new(), &[]);
    let bundle = today();
    replica.host.load_bundle(&bundle).unwrap();
    let ada = Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    // Each package's own first scenario fills it, then Today's attaches them, each change
    // committed through the workspace as a window commits it: the document holds who made what,
    // which a place that names what the person has not read asks for.
    for import in &bundle.imports {
        replica.host.root_of(&import.meta.app).unwrap();
        replica.commit();
        let first = import.meta.scenarios.first().unwrap();
        replica
            .host
            .runtime()
            .invoke_action(&ada, (first.action.as_str(), vec![]))
            .unwrap();
        replica.commit();
    }
    replica.host.root_of(&bundle.meta.app).unwrap();
    replica.commit();
    let scenario = bundle.meta.scenarios.first().unwrap();
    replica
        .host
        .runtime()
        .invoke_action(&ada, (scenario.action.as_str(), vec![]))
        .unwrap();
    replica.commit();
    let mut view = replica.view(&bundle.meta.app);
    let mut keys = Vec::new();
    keys.extend(lists(view.tree()));
    let mut shown = Vec::new();
    shown.extend(texts_of(view.tree()));
    for key in &keys {
        for i in 0..view.list_len(key).unwrap_or(0) {
            if let Some(row) = view.row(key, i).unwrap() {
                shown.extend(texts_of(row));
            }
        }
    }
    // The task's row, and Chat's unread conversation row (`general` holds a message Ada has
    // not read): the channel names itself, the hash being its icon.
    for text in ["Buy oat milk and eggs", "general"] {
        assert!(
            shown.iter().any(|shown| shown.contains(text)),
            "Today draws `{text}` with the view of the package it belongs to: {shown:?}"
        );
    }
}

#[test]
fn every_shipped_package_names_only_types_the_platform_reads() {
    let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    for entry in std::fs::read_dir(&apps).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.join("app.tl").exists() {
            continue;
        }
        let bundle = bundle_of(&dir);
        for meta in bundle
            .imports
            .iter()
            .map(|import| &import.meta)
            .chain([&bundle.meta])
        {
            let unreadable = tilolang_tilo::meta::unreadable_types(meta);
            assert!(unreadable.is_empty(), "{}: {unreadable:?}", meta.app);
        }
        // What it loads from is what it was built into.
        let out = tempfile::tempdir().unwrap();
        bundle.write(out.path()).unwrap();
        Bundle::read(out.path()).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    }
}

/// The MCP server is one of the platform's packages: a window reads the server, its grants and
/// the workspaces it reaches as `mcp.server`, and stops an agent or lets it go on through the
/// server's own verbs.
#[test]
fn the_mcp_server_is_a_platform_package_an_app_reads() {
    let platform =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let mcp = platform
        .into_iter()
        .find(|meta| meta.app == tilolang_tilo::platform_apps::MCP_SERVER_APP)
        .expect("the MCP server is one of the platform's packages");
    let root = mcp
        .named_roots
        .iter()
        .find(|root| root.published.as_deref() == Some("mcp.server"))
        .expect("its server reads under `mcp.server`");
    assert!(
        mcp.types
            .iter()
            .any(|ty| ty.id == root.ty && ty.name == "Server"),
        "the root is the server's own type"
    );
    for verb in ["stop", "go-on"] {
        // Its verbs act on the server's own type, as a resource's do.
        let id = tilolang_tilo::catalogue::action_id(
            &mcp.app,
            &tilolang_tilo::catalogue::verb_id("Server", verb),
        );
        assert!(
            mcp.actions.iter().any(|action| action.id == id),
            "the server publishes `{verb}` among {:?}",
            mcp.actions
                .iter()
                .map(|action| &action.id)
                .collect::<Vec<_>>()
        );
    }
}

/// A workspace's model providers are one of the platform's packages: a window reads the
/// providers, their models and what runs where as `models.models`, and changes them through the
/// resource's own verbs.
#[test]
fn the_workspaces_model_providers_are_a_platform_package_an_app_reads() {
    let platform =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let models = platform
        .into_iter()
        .find(|meta| meta.app == tilolang_tilo::platform_apps::MODELS_APP)
        .expect("the model providers are one of the platform's packages");
    let root = models
        .named_roots
        .iter()
        .find(|root| root.published.as_deref() == Some("models"))
        .expect("they read under `models`");
    assert!(
        models
            .types
            .iter()
            .any(|ty| ty.id == root.ty && ty.name == "Models"),
        "the root is the models' own type"
    );
    for verb in [
        "set-key",
        "remove-key",
        "remove-provider",
        "add-preset",
        "set-default",
    ] {
        let id = tilolang_tilo::catalogue::action_id(
            &models.app,
            &tilolang_tilo::catalogue::verb_id("Models", verb),
        );
        assert!(
            models.actions.iter().any(|action| action.id == id),
            "the models publish `{verb}` among {:?}",
            models
                .actions
                .iter()
                .map(|action| &action.id)
                .collect::<Vec<_>>()
        );
    }
}

/// A workspace's connected accounts are one of the platform's packages: a window reads them as
/// `accounts.accounts`, and connects, signs in again and removes them through the resource's own
/// verbs.
#[test]
fn the_workspaces_accounts_are_a_platform_package_an_app_reads() {
    let platform =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let accounts = platform
        .into_iter()
        .find(|meta| meta.app == tilolang_tilo::platform_apps::ACCOUNTS_APP)
        .expect("a workspace's accounts are one of the platform's packages");
    let root = accounts
        .named_roots
        .iter()
        .find(|root| root.published.as_deref() == Some("accounts"))
        .expect("they read under `accounts`");
    assert!(
        accounts
            .types
            .iter()
            .any(|ty| ty.id == root.ty && ty.name == "Accounts"),
        "the root is the accounts' own type"
    );
    for verb in ["add-mail", "reconnect", "remove"] {
        let id = tilolang_tilo::catalogue::action_id(
            &accounts.app,
            &tilolang_tilo::catalogue::verb_id("Accounts", verb),
        );
        assert!(
            accounts.actions.iter().any(|action| action.id == id),
            "the accounts publish `{verb}` among {:?}",
            accounts
                .actions
                .iter()
                .map(|action| &action.id)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_type_the_platform_cannot_read_is_named_with_where_it_is() {
    let bundle = today();
    // The first package with a declared field: Today uses the todo list's.
    let mut meta = bundle
        .imports
        .iter()
        .map(|import| import.meta.clone())
        .chain([bundle.meta.clone()])
        .find(|meta| meta.types.iter().any(|ty| !ty.fields.is_empty()))
        .expect("a package Today bundles declares a field");
    let field = meta
        .types
        .iter_mut()
        .flat_map(|ty| ty.fields.iter_mut())
        .next()
        .expect("a declared field");
    field.ty = "List<".to_owned();
    let unreadable = tilolang_tilo::meta::unreadable_types(&meta);
    assert_eq!(unreadable.len(), 1, "{unreadable:?}");
    assert!(unreadable[0].contains("List<"), "{unreadable:?}");
}

/// A payload that ships no module is a package of another kind — a Rust app's, which keeps its
/// code in `bin/` — and reads as no tilolang package rather than as a broken one.
#[test]
fn a_payload_without_a_module_is_no_tilolang_package() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        "[package]\nid = \"so.tilo.example\"\n",
    )
    .unwrap();
    assert!(matches!(
        Bundle::read(dir.path()),
        Err(tilolang_tilo::bundle::BundleError::NotAModule(_))
    ));
}

const SOUND_APP: &str = r#"app so.tilo.test.sound "Sound" (icon: .music, category: .utility)

/// What the demo plays.
type Player {
    name: Text = ""
}

root Player

default view player for Player

/// The player's window: it names the file a view shows beside the one only a demo does.
view player(p: Player) {
    ui.Text("pkg:media/shown.txt")
}

/// A demo that names a sound of its own.
scenario "one" {
    let p = root Player
    p.name = "pkg:media/only-scenario.txt"
}
"#;

/// A package's files under `media/` travel with it; the ones only a scenario names are left out of
/// the payload a person installs, since an install starts empty and shows no demo. A development
/// run carries them all, because the window it opens stages the demos.
#[test]
fn an_install_leaves_out_the_files_only_a_scenario_names() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("sound");
    std::fs::create_dir_all(package.join("media")).unwrap();
    std::fs::write(package.join("app.tl"), SOUND_APP).unwrap();
    std::fs::write(package.join("media/shown.txt"), b"shown").unwrap();
    std::fs::write(package.join("media/only-scenario.txt"), b"demo").unwrap();
    let published = tilolang_tilo::catalogue::published_packages(&[]).unwrap();
    let built = tilolang::source::compile_package(
        &package,
        &[],
        &published,
        &mut tilolang::OsEntropy::default(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let bundle = tilolang_tilo::bundle::of_build(&built).pop().unwrap();
    let flags: Vec<(String, bool)> = bundle
        .assets
        .iter()
        .map(|asset| (asset.name.clone(), asset.scenario_only))
        .collect();
    assert_eq!(
        flags,
        [
            ("media/only-scenario.txt".to_owned(), true),
            ("media/shown.txt".to_owned(), false),
        ]
    );

    let installed = dir.path().join("installed");
    bundle.write_installed(&installed).unwrap();
    assert!(
        installed.join("media/shown.txt").is_file(),
        "a view shows it"
    );
    assert!(
        !installed.join("media/only-scenario.txt").exists(),
        "an install carries nothing only a demo names"
    );

    let developed = dir.path().join("developed");
    bundle.write(&developed).unwrap();
    assert!(
        developed.join("media/only-scenario.txt").is_file(),
        "a development run stages the demo that names it"
    );
}
