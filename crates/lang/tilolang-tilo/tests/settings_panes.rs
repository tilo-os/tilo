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

use crate::fixtures::labelled;
use tilo_action_policy::MemberAccess;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ViewInstance};

const APP: &str = "so.tilo.settings";

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
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        move || access.clone(),
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    // The platform's packages it uses, as a window's process loads them: what a pane reads of the
    // workspace or the device is a resource their readings and actions come from.
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    crate::fixtures::settings_sections(&host);
    (host, compiled)
}

/// The pane the window shows, as the app's own document holds it.
fn pane(host: &Host) -> String {
    let root = host.root_of(APP).expect("the app's root");
    match host.runtime().read(&root, "pane").expect("a pane") {
        Value::Text(text) => text,
        other => panic!("not text: {other:?}"),
    }
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

/// The lines a view shows, its text as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
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

/// One invocation as it was sent, with its arguments by name.
pub(crate) struct Sent(pub(crate) Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// What a host keeps of the workspace's content reads in mebibytes, the unit its owners set the
/// limits in, and only an owner is offered to set them.
#[test]
fn the_content_pane_shows_what_the_host_keeps_in_mebibytes() {
    use tilo_action_policy::{Role, WorkspacePolicy};

    // A member who is no owner of the workspace: they read its limits and change none.
    let mut policy = WorkspacePolicy::new("team", "bo");
    policy.members.insert("ada".to_owned(), Role::Editor);
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

    view.press(&labelled(view.tree(), "ui.Button", "Change"))
        .expect("the owner opens the limits");
    view.poll_patches().expect("the fields draw");
    assert!(
        texts(&view).iter().any(|text| text == "New limits"),
        "{:?}",
        texts(&view)
    );
    view.press(&labelled(view.tree(), "ui.Button", "Save limits"))
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
    view.press(&labelled(view.tree(), "ui.Button", "Add the word"))
        .expect("the word is added");
    // A word kept is removed by the one written for it, in the language it was kept in.
    view.press(&labelled(view.tree(), "ui.Button", "Remove"))
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
    view.press(&use_it[1]).expect("German is chosen");

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
    view.press(&labelled(view.tree(), "ui.Button", "Back up"))
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
    view.fire(&key, "activate").expect("the tile is chosen");
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
        (tilolang_tilo::platform_apps::SECTIONS_APP, "set-wallpaper"),
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
