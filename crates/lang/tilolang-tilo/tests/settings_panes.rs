//! Settings' app-level actions show the pane a search page or setting names, as search's rows
//! run them from outside the window, on the app's published resource.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crate::fixtures::invoker::Answered as _;
use crate::fixtures::{Sent, labelled, settings_pane as pane, texts};
use tilo_action_policy::MemberAccess;
use tilolang_runtime::delegate::Invocation;
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ViewInstance};

const APP: &str = "so.tilo.settings";

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The app as it ships: its source, its ids and the platform's packages it reads through.
pub(crate) fn settings() -> (Host, tilolang::Compiled) {
    settings_with(None)
}

/// [`settings`], the person at the window deciding by `access` (an owner's, say): what a pane
/// offers them and what its verbs reach.
fn settings_with(access: Option<MemberAccess>) -> (Host, tilolang::Compiled) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let policy = access
        .as_ref()
        .map(|access| std::sync::Arc::clone(&access.policy));
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        move || access.clone(),
    )
    .expect("a host");
    // A member's role is read from the workspace's members' resource: the people the policy names
    // are in it, as the workspace's add-member action puts them.
    if let Some(policy) = &policy {
        crate::fixtures::stage_people(&host, policy);
    }
    host.load_compiled(&compiled).expect("the package loads");
    // The platform's packages it uses, as a window's process loads them: what a pane reads of the
    // workspace or the device is a resource their readings and actions come from.
    tilolang_tilo::platform::load(host.runtime(), &compiled.meta.uses);
    crate::fixtures::settings_sections(&host);
    (host, compiled)
}

/// Run the app-level action `name` with `args`, as a search row does from outside the window.
pub(crate) fn invoke(host: &Host, name: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the app publishes no `{name}`"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (id.as_str(), args),
        )
        .unwrap_or_else(|why| panic!("{name}: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn a_setting_opens_the_pane_it_is_on_and_a_page_its_own() {
    let (host, _) = settings();
    let _ = host.root_of(APP).expect("the app's root");
    assert_eq!(pane(&host), "appearance", "the window starts on Appearance");

    invoke(&host, "show_setting", vec![Value::text("keyboard.layout")]);
    assert_eq!(pane(&host), "input", "a keyboard setting is on Input");

    invoke(
        &host,
        "show_setting",
        vec![Value::text("a11y.high_contrast")],
    );
    assert_eq!(pane(&host), "accessibility");

    invoke(
        &host,
        "show_setting",
        vec![Value::text("notifications.banners")],
    );
    assert_eq!(pane(&host), "notifications");

    invoke(&host, "open_pane", vec![Value::text("external_accounts")]);
    assert_eq!(pane(&host), "accounts", "the index's page slug is Accounts");

    invoke(&host, "open_pane", vec![Value::text("keyboard")]);
    assert_eq!(pane(&host), "input");

    invoke(&host, "open_pane", vec![Value::text("displays")]);
    assert_eq!(pane(&host), "displays");
}

/// The pane `name` opened as its own view: the window's content area is laid out for its size,
/// which a host without a window has none of, so what the pane draws is read from the pane.
pub(crate) fn open_view(host: &Host, name: &str) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the {name} is declared"))
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .expect("the pane opens");
    view.poll_patches().expect("the pane draws");
    view
}

/// The content pane opened as its own view.
pub(crate) fn open_content_pane(host: &Host) -> ViewInstance {
    open_view(host, "content_pane")
}

/// The settings window opened as its own view, on the pane its state has in front: the whole
/// window, as the person sees it, so a pane found by its words reads as it does there.
pub(crate) fn open_window(host: &Host) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "settings_window")
        .unwrap_or_else(|| panic!("the window is declared"))
        .id
        .clone();
    let root = host.root_of(APP).expect("the app's root");
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            vec![Value::resource(&root)],
        )
        .expect("the window opens");
    view.poll_patches().expect("the window draws");
    view
}

/// What a person sets the limits to, past the default: a size in bytes, unset for a member.
fn limits(host: &Host) {
    use tilolang_tilo::content::WorkspaceContent;

    host.give_content(&WorkspaceContent {
        file_bytes: 25 * 1_048_576,
        member_bytes: 250 * 1_048_576,
        hourly_bytes: 250 * 1_048_576,
        refusals: Vec::new(),
        host_person: None,
    });
}

/// What a host keeps of the workspace's content reads in mebibytes, the unit its owners set the
/// limits in, and only an owner is offered to set them.
#[test]
fn the_content_pane_shows_what_the_host_keeps_in_mebibytes() {
    use tilo_action_policy::{Role, WorkspacePolicy};

    // A member who is no owner of the workspace: they read its limits and change none.
    let mut policy = WorkspacePolicy::new("team", "bo");
    policy.members.insert("ada".to_owned(), Role::Editor.into());
    let (host, _) = settings_with(Some(MemberAccess::person(policy, "ada")));
    limits(&host);
    invoke(&host, "open_pane", vec![Value::text("content")]);
    assert_eq!(pane(&host), "content");
    let view = open_content_pane(&host);
    let shown = texts(&view);
    assert!(shown.contains(&"25 MiB".to_owned()), "{shown:?}");
    assert!(
        shown.iter().filter(|text| *text == "250 MiB").count() == 2,
        "a member's share and their hour: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|text| text == "Only the workspace's owners can change these."),
        "a member reads them and changes none: {shown:?}"
    );
    assert!(
        view.tree().of_kind("ui.Button").is_empty(),
        "a member is offered no limit to change: {:?}",
        texts(&view)
    );
}

/// A limit nobody set reads as no limit: the workspace's folders are its storage, not a
/// ceiling's, and the pane says so for each of the three.
#[test]
fn a_limit_nobody_set_reads_as_no_limit() {
    use tilo_action_policy::WorkspacePolicy;
    use tilolang_tilo::content::WorkspaceContent;

    let owner = MemberAccess::person(WorkspacePolicy::new("team", "ada"), "ada");
    let (host, _) = settings_with(Some(owner));
    host.give_content(&WorkspaceContent {
        file_bytes: 0,
        member_bytes: 0,
        hourly_bytes: 0,
        refusals: Vec::new(),
        host_person: None,
    });
    invoke(&host, "open_pane", vec![Value::text("content")]);
    let view = open_content_pane(&host);
    let shown = texts(&view);
    assert_eq!(
        shown.iter().filter(|text| *text == "No limit").count(),
        3,
        "a file, a member's files and their hour: {shown:?}"
    );
}

/// An owner changes the limits where the workspace keeps them: the pane's Save sends the
/// workspace's own `content.set-policy` with the sizes it shows, in bytes.
#[test]
fn an_owner_sets_the_limits_the_pane_shows() {
    use tilo_action_policy::WorkspacePolicy;

    let owner = MemberAccess::person(WorkspacePolicy::new("team", "ada"), "ada");
    let (host, _) = settings_with(Some(owner));
    limits(&host);
    invoke(&host, "open_pane", vec![Value::text("content")]);
    let mut view = open_content_pane(&host);

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press_answering(&host, &me(), &labelled(view.tree(), "ui.Button", "Change"))
        .expect("the owner opens the limits");
    view.poll_patches().expect("the fields draw");
    assert!(
        texts(&view).iter().any(|text| text == "New limits"),
        "{:?}",
        texts(&view)
    );
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Save limits"),
    )
    .expect("the owner saves them");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the workspace: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("tilo.workspace", "set-policy"),
        "the workspace's own action, as its surface publishes it"
    );
    // An unset limit is sent as the optional it is, holding the bytes.
    let arg = |wanted: &str| {
        args.iter()
            .find(|(named, _)| named == wanted)
            .map(|(_, value)| match value {
                Value::Opt(Some(inner)) => (**inner).clone(),
                other => other.clone(),
            })
    };
    assert_eq!(arg("file_bytes"), Some(Value::Int(25 * 1_048_576)));
    assert_eq!(arg("member_bytes"), Some(Value::Int(250 * 1_048_576)));
    assert_eq!(arg("hourly_bytes"), Some(Value::Int(250 * 1_048_576)));
}

/// The spelling pane keeps the person's own words: it shows the words the dictionary section
/// holds, adds the word typed, and removes one as written, each through the section's published
/// verbs — the same ones the word processor, the on-screen keyboard and an agent change the
/// dictionary through, so every surface spells with one dictionary.
#[test]
fn the_spelling_pane_keeps_words_in_the_personal_dictionary() {
    let (host, _) = settings();
    let dictionary = ResourceRef::parse_id(
        "settings/personal-dictionary",
        "so.tilo.settings.sections#PersonalDictionary",
    )
    .expect("the dictionary's resource");
    host.runtime()
        .set_reading(
            &dictionary,
            r#"{"words": {"revision": 3, "words": [
                {"spelling": "Tilo", "language": null},
                {"spelling": "Élodie", "language": "fr"}
            ]}}"#,
        )
        .expect("the words the person kept");
    invoke(&host, "open_pane", vec![Value::text("spelling")]);
    let view = open_view(&host, "spelling_pane");
    let shown = texts(&view);
    assert!(shown.contains(&"Tilo".to_owned()), "{shown:?}");
    assert!(shown.contains(&"Élodie".to_owned()), "{shown:?}");
    assert!(
        shown.contains(&"In fr".to_owned()) && shown.contains(&"Every language".to_owned()),
        "each word says the language it is used in: {shown:?}"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    // The word typed is added as written, for every language while no language is given.
    let word_field = view
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| {
            node.props.get("placeholder") == Some(&Value::text("A word, spelled as you write it"))
        })
        .expect("the word field")
        .key
        .clone();
    view.edit(&word_field, &Value::text("Kiitos"))
        .expect("the word typed");
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Add the word"),
    )
    .expect("the word is added");
    // A word kept is removed by the one written for it, in the language it was kept in.
    view.press_answering(&host, &me(), &labelled(view.tree(), "ui.Button", "Remove"))
        .expect("the first word is removed");

    let sent = sent.borrow();
    let [added, removed] = sent.as_slice() else {
        panic!("the pane's two changes: {sent:?}");
    };
    for (invocation, verb) in [(added, "add"), (removed, "remove")] {
        let Invocation::Action {
            app, name, args, ..
        } = invocation
        else {
            panic!("a verb of the dictionary: {invocation:?}");
        };
        assert_eq!(
            (app.as_str(), name.as_str()),
            ("so.tilo.settings.sections", verb),
            "the section's own verb, as its surface publishes it"
        );
        let arg = |wanted: &str| {
            args.iter()
                .find(|(named, _)| named == wanted)
                .map(|(_, value)| value.clone())
        };
        if verb == "add" {
            assert_eq!(arg("spelling"), Some(Value::text("Kiitos")));
            assert_eq!(
                arg("language"),
                Some(Value::Opt(None)),
                "no language given keeps the word for every language"
            );
        } else {
            assert_eq!(arg("spelling"), Some(Value::text("Tilo")));
            assert_eq!(arg("language"), Some(Value::Opt(None)));
        }
    }
}

/// The Region pane offers the languages this desktop speaks, each in its own words, and asking
/// for one is the appearance section's own `choose-language` — the same verb a shortcut, an agent
/// and a script change the language through.
#[test]
fn the_region_pane_offers_the_languages_the_desktop_speaks() {
    let (host, _) = settings();
    let languages =
        ResourceRef::parse_id("settings/languages", "so.tilo.settings.sections#Languages")
            .expect("the languages' resource");
    host.runtime()
        .set_reading(
            &languages,
            r#"{"languages": [{"tag": "en", "name": "English"}, {"tag": "de", "name": "Deutsch"}]}"#,
        )
        .expect("the languages the desktop speaks");
    let appearance = ResourceRef::parse_id(
        "settings/appearance",
        "so.tilo.settings.sections#Appearance",
    )
    .expect("the appearance's resource");
    host.runtime()
        .set_reading(&appearance, "{}")
        .expect("the appearance the person chose");
    invoke(&host, "open_pane", vec![Value::text("region")]);
    let view = open_view(&host, "region_pane");
    let shown = texts(&view);
    let kinds: Vec<&str> = crate::fixtures::nodes(view.tree())
        .into_iter()
        .map(|node| node.kind.as_str())
        .collect();
    assert!(
        shown.contains(&"English".to_owned()),
        "{shown:?} kinds={kinds:?}"
    );
    assert!(shown.contains(&"Deutsch".to_owned()), "{shown:?}");

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    // The second language's button, after the row that follows the device's.
    let use_it: Vec<String> = view
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .filter(|node| node.props.get("label") == Some(&Value::text("Use it")))
        .map(|node| node.key.clone())
        .collect();
    assert_eq!(use_it.len(), 2, "one for each language: {shown:?}");
    view.press_answering(&host, &me(), &use_it[1])
        .expect("German is chosen");

    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the appearance: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.settings.sections", "choose-language"),
        "the appearance section's own verb, as its surface publishes it"
    );
    assert_eq!(
        args.iter()
            .find(|(named, _)| named == "language")
            .map(|(_, value)| value.clone()),
        Some(Value::Opt(Some(Box::new(Value::text("de"))))),
        "the language by the tag a setting holds"
    );
}

/// About carries this computer's identity to another one: the backup is the devices resource's own
/// verb, the same one the setup screens and an agent ask for, so a person writes one backup and
/// every caller reaches it the same way.
#[test]
fn the_about_pane_offers_to_back_up_this_identity() {
    let (host, _) = settings();
    invoke(&host, "open_pane", vec![Value::text("about")]);
    let view = open_view(&host, "about_pane");
    let shown = texts(&view);
    assert!(
        shown.contains(&"Back up your identity".to_owned()),
        "{shown:?}"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.press_answering(&host, &me(), &labelled(view.tree(), "ui.Button", "Back up"))
        .expect("the backup is asked for");

    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, target, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the devices: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "back-up-identity"),
        "the devices resource's own verb, as its surface publishes it"
    );
    assert_eq!(
        target.as_ref().map(ResourceRef::id_string).as_deref(),
        Some("session:devices"),
        "asked of this computer's devices, where the identity's backup is published"
    );
}

/// The session's reading of the pictures the image ships, as a host hands it in.
fn wallpapers(host: &Host, reading: serde_json::Value) {
    let resource =
        tilolang_runtime::ResourceRef::parse_id("session:wallpaper", "so.tilo.device#Wallpaper")
            .expect("the wallpaper resource");
    assert!(
        host.runtime()
            .set_reading(&resource, &reading.to_string())
            .expect("the reading is set"),
        "the session serves a wallpaper reading"
    );
}

/// The person's appearance at `settings/appearance`, as a host hands it in.
fn appearance(host: &Host, reading: serde_json::Value) {
    let resource = tilolang_runtime::ResourceRef::parse_id(
        "settings/appearance",
        "so.tilo.settings.sections#Appearance",
    )
    .expect("the appearance resource");
    host.runtime()
        .set_reading(&resource, &reading.to_string())
        .expect("the reading is set");
}

/// The card the pane lays out with `title`, among the grid's cards as the page stands them.
fn tile<'a>(view: &'a ViewInstance, title: &str) -> &'a tilolang_runtime::UiNode {
    view.tree()
        .of_kind("ui.MediaCard")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text(title)))
        .unwrap_or_else(|| {
            let shown: Vec<_> = view
                .tree()
                .of_kind("ui.MediaCard")
                .into_iter()
                .map(|node| node.props.get("title").cloned())
                .collect();
            panic!("a tile called {title}; the cards read {shown:?}")
        })
}

/// Whether a card is the one in use: a tl flag reaches the card as the number it is.
fn chosen(node: &tilolang_runtime::UiNode) -> bool {
    match node.props.get("selected") {
        Some(Value::Bool(on)) => *on,
        Some(Value::Int(on)) => *on != 0,
        other => panic!("a card's selected reads {other:?}"),
    }
}

/// The theme each of the pane's drawings draws in, `None` for the one that follows the device.
fn drawn_themes(tree: &tilolang_runtime::UiNode) -> Vec<Option<String>> {
    tree.of_kind("ui.Drawing")
        .into_iter()
        .map(|node| match node.props.get("theme") {
            Some(Value::Text(text)) => Some(text.clone()),
            Some(Value::Opt(Some(held))) => match held.as_ref() {
                Value::Text(text) => Some(text.clone()),
                other => panic!("a theme reads {other:?}"),
            },
            Some(Value::Opt(None)) | None => None,
            Some(other) => panic!("a theme reads {other:?}"),
        })
        .collect()
}

/// The pictures the image ships are a grid of previews with what each credits, and choosing one
/// sets `appearance.wallpaper` through the section's own verb.
#[test]
fn the_wallpapers_the_image_ships_are_offered_and_chosen_through_the_setting() {
    let (host, _) = settings();
    wallpapers(
        &host,
        serde_json::json!({
            "picture": "",
            "shipped": "alps",
            "catalogue": [
                {
                    "id": "alps",
                    "title": "The Alps from Grenoble",
                    "author": "Dktue",
                    "license": "CC0-1.0",
                    "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
                    "tone": "mid",
                    "thumbnail": "",
                },
                {
                    "id": "andromeda",
                    "title": "Andromeda Galaxy",
                    "author": "Brody Wesner",
                    "license": "CC0-1.0",
                    "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
                    "tone": "dark",
                    "thumbnail": "",
                },
            ],
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("appearance")]);
    let mut view = open_view(&host, "appearance_pane");
    view.set_lazy_rows(false).expect("the pictures stand");
    // A tile's words are its title and the two lines under it.
    let words_of = |node: &tilolang_runtime::UiNode| -> Vec<String> {
        node.props
            .values()
            .flat_map(|value| match value {
                Value::Text(text) => vec![text.clone()],
                Value::List(items) => items
                    .iter()
                    .filter_map(|item| match item {
                        Value::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    };
    for words in ["Andromeda Galaxy", "Brody Wesner", "CC0-1.0"] {
        assert!(
            words_of(tile(&view, "Andromeda Galaxy")).contains(&words.to_owned()),
            "{words} shows on its tile"
        );
    }
    // The picture the image ships is chosen while the person has chosen none.
    assert!(
        chosen(tile(&view, "The Alps from Grenoble")),
        "the shipped picture is the one in use"
    );
    assert!(
        !chosen(tile(&view, "Andromeda Galaxy")),
        "a picture the person has not chosen is not the one in use"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    let key = tile(&view, "Andromeda Galaxy").key.clone();
    view.fire_answering(&host, &me(), &key, "activate")
        .expect("the tile is chosen");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            name,
            target,
            args,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the settings: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        (tilolang_tilo::platform::SECTIONS_APP, "set-wallpaper"),
        "the appearance's own setter, as its surface publishes it"
    );
    assert_eq!(
        target.as_ref().map(|held| held.doc.as_str()),
        Some("settings/appearance"),
        "the setting the picture is chosen on"
    );
    let value = args
        .iter()
        .find(|(named, _)| named == "value")
        .map(|(_, value)| value.clone());
    assert_eq!(
        value,
        Some(Value::Opt(Some(Box::new(Value::text("andromeda"))))),
        "the picture the person chose, as the optional the setting holds"
    );
}

/// The pane previews the theme each mode shows, beside the one in use, and an edit to which theme
/// a mode shows reaches the preview.
#[test]
fn the_theme_previews_draw_the_modes_themes_and_follow_a_change() {
    let (host, _) = settings();
    appearance(
        &host,
        serde_json::json!({
            "mode": "dark",
            "light_theme": "tilo-light",
            "dark_theme": "tilo-dark",
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("appearance")]);
    let mut view = open_view(&host, "appearance_pane");
    let themes = drawn_themes(view.tree());
    assert!(
        themes.contains(&Some("tilo-light".to_owned())),
        "the light mode's theme is drawn: {themes:?}"
    );
    assert!(
        themes.contains(&Some("tilo-dark".to_owned())),
        "the dark mode's theme is drawn: {themes:?}"
    );
    assert!(
        themes.contains(&None),
        "the theme in use follows the device: {themes:?}"
    );

    appearance(
        &host,
        serde_json::json!({
            "mode": "dark",
            "light_theme": "tilo-light",
            "dark_theme": "paper-dark",
        }),
    );
    view.poll_patches().expect("the preview takes the change");
    let themes = drawn_themes(view.tree());
    assert!(
        themes.contains(&Some("paper-dark".to_owned())),
        "the dark mode's new theme is drawn: {themes:?}"
    );
    assert!(
        !themes.contains(&Some("tilo-dark".to_owned())),
        "the one it showed before is gone: {themes:?}"
    );
}

/// The battery's health, its cycles and its capacity are the battery service's reading, shown as
/// the person's words for them.
#[test]
fn the_battery_pane_shows_health_cycles_and_capacity() {
    let (host, _) = settings();
    crate::fixtures::set_reading(
        &host,
        "power:battery",
        &serde_json::json!({
            "present": true,
            "percentage": 73,
            "state": "discharging",
            "on_battery": true,
            "minutes_left": 214,
            "health": 87,
            "cycles": 142,
            "capacity_wh": 57,
        }),
    );
    let view = open_view(&host, "power_pane");
    let shown = texts(&view);
    for words in [
        "Battery health",
        "87% of a new pack; wearing as expected",
        "Charge cycles",
        "142; each is a full charge and discharge",
        "Capacity",
        "57 Wh when new",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in the battery pane: {shown:?}"
        );
    }
}

/// The update page offers the download schedule and whether a metered connection may be used,
/// read from the machine's update resource, and each choice is the resource's own verb.
#[test]
fn the_updates_pane_offers_the_schedule_and_metered_choice() {
    let (host, _) = settings();
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    crate::fixtures::set_reading(
        &host,
        "system-update",
        &serde_json::json!({
            "booted": null,
            "staged": null,
            "rollback": null,
            "applied": false,
            "desktop_pending": false,
            "restart_needed": false,
            "failed": null,
            "updating": false,
            "pending": [],
            "problem": null,
            "firmware": null,
            "fell_back": [],
            "schedule": "weekly",
            "allow_metered": false,
        }),
    );
    let view = open_view(&host, "updates_pane");
    let shown = texts(&view);
    for words in [
        "Update schedule",
        "The image's own",
        "Daily",
        "Weekly",
        "Metered connections",
        "Wait for one that is not",
        "Download anyway",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in the updates pane: {shown:?}"
        );
    }

    view.press_answering(&host, &me(), &labelled(view.tree(), "ui.Segment", "Daily"))
        .expect("the schedule is chosen");
    let sent = sent.borrow();
    let (app, name, schedule) = sent
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action {
                app, name, args, ..
            } => args
                .iter()
                .find(|(named, _)| named == "schedule")
                .map(|(_, value)| (app, name, value.clone())),
            Invocation::Edit { .. } => None,
        })
        .expect("the choice is the update resource's verb");
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "set-schedule")
    );
    assert_eq!(schedule, Value::text("daily"));
}

/// A captive portal's page opens in the app that opens web addresses, from the network that asks
/// for it.
#[test]
fn a_captive_portals_sign_in_page_opens() {
    let (host, _) = settings();
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    crate::fixtures::set_reading(
        &host,
        "network",
        &serde_json::json!({
            "airplane_mode": false,
            "connectivity": "portal",
            "portal_uri": "https://portal.example/sign-in",
            "portal_problem": null,
            "wifi": {
                "available": true,
                "on": true,
                "hardware_blocked": false,
                "networks": [],
            },
            "ask_before_open_networks": false,
            "wired": null,
            "saved": [],
            "mobile": {"available": false, "on": false, "profile": null},
            "problem": null,
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("network")]);
    let view = open_window(&host);
    let shown = texts(&view);
    for words in [
        "Sign in to this network",
        "Open sign-in page",
        "The network asks for a sign-in page before it lets traffic out",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows on the network page: {shown:?}"
        );
    }
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Open sign-in page"),
    )
    .expect("the page opens");
    assert_eq!(
        sent.borrow().last().map(|invocation| match invocation {
            Invocation::Action { app, name, .. } => (app.as_str(), name.as_str()),
            Invocation::Edit { .. } => ("", ""),
        }),
        Some(("so.tilo.device", "open-link"))
    );
}

/// A saved network's hardware address, its wake-over-LAN and its IPv6 settings are shown on its
/// own page, as the network's reading holds them.
#[test]
fn a_saved_network_shows_its_address_wake_and_ipv6() {
    let (host, _) = settings();
    crate::fixtures::set_reading(
        &host,
        "network",
        &serde_json::json!({
            "airplane_mode": false,
            "connectivity": "full",
            "portal_uri": null,
            "portal_problem": null,
            "wifi": {
                "available": true,
                "on": true,
                "hardware_blocked": false,
                "networks": [{
                    "name": "Studio",
                    "known": true,
                    "connected": false,
                    "secure": true,
                    "signal": 4,
                }],
            },
            "ask_before_open_networks": false,
            "wired": null,
            "saved": [{
                "name": "Studio",
                "profile": "uuid-studio",
                "kind": "wifi",
                "autoconnect": true,
                "priority": 0,
                "metered": "automatic",
                "hidden": false,
                "open": false,
                "ipv4": null,
                "ipv6": {
                    "method": "manual",
                    "addresses": ["2001:db8::10/64"],
                    "gateway": "2001:db8::1",
                    "dns": ["2001:db8::53"],
                },
                "mac": "random",
                "wake": {
                    "on": true,
                    "interface": "wlan0",
                    "supported": true,
                    "applied": true,
                    "problem": null,
                },
                "enterprise": null,
            }],
            "mobile": {"available": false, "on": false, "profile": null},
            "problem": null,
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("network")]);
    let mut view = open_window(&host);
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "This network"),
    )
    .expect("the network's page opens");
    view.poll_patches().expect("the page redraws");
    let shown = texts(&view);
    for words in [
        "The address it shows",
        "Random each connection",
        "Wake over this network",
        "Another device on this network can wake this computer from sleep",
        "IPv6",
        "2001:db8::10/64",
        "Apply IPv6 settings",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows on the saved network's page: {shown:?}"
        );
    }
}

/// A trusted accessory says it connects by itself, and the switch asks the accessory's own verb
/// to stop trusting it.
#[test]
fn a_trusted_accessory_offers_to_ask_first() {
    let (host, _) = settings();
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    crate::fixtures::set_reading(
        &host,
        "bluetooth",
        &serde_json::json!({
            "available": true,
            "on": true,
            "accessories": [{
                "name": "Headphones",
                "address": "AA:BB",
                "battery": 80,
                "signal": -52,
                "connected": false,
                "trusted": true,
            }],
            "nearby": [],
            "discovering": false,
            "pending": [],
            "problem": null,
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("bluetooth")]);
    let view = open_window(&host);
    assert!(
        texts(&view).iter().any(|text| text == "Connects by itself"),
        "a trusted accessory says so: {:?}",
        texts(&view)
    );
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Connects by itself"),
    )
    .expect("the trust is taken back");
    let sent = sent.borrow();
    let trusted = sent
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action { name, args, .. } if name == "set-trusted" => args
                .iter()
                .find(|(named, _)| named == "trusted")
                .map(|(_, value)| value.clone()),
            _ => None,
        })
        .expect("the accessory's own verb is asked");
    assert_eq!(trusted, Value::Bool(false));
}

/// The shortcuts list names every shortcut in words, including the numbered window and strip
/// shortcuts, and the menu offers those words.
#[test]
fn the_shortcuts_list_names_every_shortcut_in_words() {
    let (host, _) = settings();
    crate::fixtures::set_reading(
        &host,
        "settings/shortcuts-table",
        &serde_json::json!({
            "bindings": [
                {"id": "search", "chord": "Super+Space"},
                {"id": "focus-group-1", "chord": "Ctrl+Super+1"},
                {"id": "focus-strip-2", "chord": "Super+2"},
            ],
            "actions": [],
            "commands": [],
        }),
    );
    let view = open_view(&host, "shortcuts_pane");
    let shown = texts(&view);
    for words in ["Search", "Window 1", "Strip 2"] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in the shortcuts list: {shown:?}"
        );
    }
    let options = view
        .tree()
        .of_kind("ui.Dropdown")
        .into_iter()
        .find_map(|node| match node.props.get("options") {
            Some(Value::List(items)) => Some(
                items
                    .iter()
                    .filter_map(|item| match item {
                        Value::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .expect("the shortcut menu lists its choices");
    assert!(
        options.contains(&"Window 1".to_owned()) && options.contains(&"Strip 2".to_owned()),
        "the menu names the shortcuts in words: {options:?}"
    );
}

/// The displays page offers automatic brightness only where an ambient-light sensor is; without
/// one it says so instead of offering a setting nothing reads.
#[test]
fn the_displays_pane_hides_automatic_brightness_without_a_sensor() {
    let displays = |sensor: bool| {
        serde_json::json!({
            "displays": [],
            "mode": "extend",
            "pending": 0,
            "pending_until_unix_ms": null,
            "focused": null,
            "level": null,
            "ambient_sensor": sensor,
            "bar_reached": false,
            "bar_covered": false,
            "dock_reached": false,
            "dock_covered": false,
            "trusted_tools": [],
        })
    };
    let devices = serde_json::json!({
        "devices": [],
        "access": [],
        "playing": null,
        "chosen": null,
        "sounds": [],
        "routes": [],
        "asks": [],
        "signed_in": false,
        "workspaces": [],
        "workspace": "",
        "picked": false,
        "waiting": false,
        "held_back": "",
        "refused": "",
        "connection": null,
    });
    for (sensor, offered) in [(false, false), (true, true)] {
        let (host, _) = settings();
        crate::fixtures::reading_at(
            &host,
            "session:displays",
            "so.tilo.device#Displays",
            displays(sensor),
        );
        crate::fixtures::reading_at(
            &host,
            "session:devices",
            "so.tilo.device#Devices",
            devices.clone(),
        );
        invoke(&host, "open_pane", vec![Value::text("displays")]);
        let view = open_window(&host);
        let shown = texts(&view);
        let note = shown
            .iter()
            .any(|text| text == "No ambient-light sensor detected on this device");
        assert_eq!(
            note, !offered,
            "a machine with a sensor offers automatic brightness, and one without says why not: \
             {shown:?}"
        );
    }
}

/// The time zones are a searchable list from the machine's own tzdata, each a press to use it,
/// and the chosen zone is marked on a map.
#[test]
fn the_time_zones_are_searchable_and_the_chosen_one_is_mapped() {
    let (host, _) = settings();
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    crate::fixtures::reading_at(
        &host,
        "time-zone",
        "so.tilo.device#TimeZone",
        serde_json::json!({
            "zone": "Europe/Amsterdam",
            "automatic": false,
            "network_time": true,
            "synchronized": true,
            "zones": [
                {"name": "Europe/Amsterdam", "latitude": 52.3667, "longitude": 4.9},
                {"name": "Asia/Tokyo", "latitude": 35.6544, "longitude": 139.7447},
            ],
            "pending": [],
            "problem": null,
        }),
    );
    crate::fixtures::reading_at(
        &host,
        "session:time",
        "so.tilo.device#Time",
        serde_json::json!({
            "clock": "12:00",
            "year": 2026,
            "month": 11,
            "day": 28,
            "weekday": 6,
            "week_start": 1,
            "month_name": "November",
            "days_in_month": 30,
            "first_weekday": 7,
            "weekdays": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
            "weekday_names": [
                "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday",
            ],
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("time")]);
    let view = open_window(&host);
    let shown = texts(&view);
    for words in [
        "Search the time zones",
        "Europe/Amsterdam",
        "Asia/Tokyo",
        "The mark on the map is where the zone's clock is kept.",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in the date and time pane: {shown:?}"
        );
    }
    assert!(
        view.tree()
            .of_kind("ui.Drawing")
            .iter()
            .any(|node| node.props.get("label")
                == Some(&Value::text("The time zone's place on the map"))),
        "the map is drawn: {}",
        crate::fixtures::drawn(view.tree())
    );

    let tokyo = view
        .tree()
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text("Asia/Tokyo")))
        .expect("the zone is listed")
        .key
        .clone();
    view.fire_answering(&host, &me(), &tokyo, "activate")
        .expect("the zone is used");
    let sent = sent.borrow();
    let zone = sent
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action { name, args, .. } if name == "set-zone" => args
                .iter()
                .find(|(named, _)| named == "zone")
                .map(|(_, value)| value.clone()),
            _ => None,
        })
        .expect("the zone is the time resource's own verb");
    assert_eq!(zone, Value::text("Asia/Tokyo"));
}

/// Clearing the apps' caches is previewed app by app, each by the bytes it takes and the files
/// it holds, under the whole it gives back.
#[test]
fn the_apps_cache_preview_names_each_app_before_clearing_it() {
    let (host, _) = settings();
    crate::fixtures::reading_at(
        &host,
        "storage",
        "so.tilo.device#Storage",
        serde_json::json!({
            "disk": null,
            "residency": [],
            "residency_error": null,
            "cache_policy": null,
            "kept": [],
            "reclaimable": [],
            "reclaimable_bytes": 0,
            "sync_error": null,
            "categories": [],
            "largest": [],
            "cache_bytes": 1100,
            "app_cache": {
                "items": [
                    {"app": "so.tilo.mail", "bytes": 500, "files": 3},
                    {"app": "so.tilo.notes", "bytes": 300, "files": 2},
                ],
                "total_bytes": 1100,
            },
            "scanned": true,
            "read_error": null,
            "pending": [],
            "problem": null,
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("storage")]);
    let view = open_window(&host);
    let shown = texts(&view);
    for words in [
        "Clearing gives back 1 KB from 2 apps.",
        "so.tilo.mail",
        "500 B in 3 files",
        "so.tilo.notes",
        "300 B in 2 files",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in the cache preview: {shown:?}"
        );
    }
}
