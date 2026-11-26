//! Windows Apps end to end: this computer's Windows programs and appliance over the device's
//! `windows-apps` reading, the pages each opens onto, and the actions they offer.

use crate::fixtures::Pane;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::blob::FileValue;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::{Host, RootView};

const SOURCE: &str = include_str!("../../../../apps/tl/windows/app.tl");

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn windows() -> ResourceRef {
    ResourceRef::parse_id("windows-apps", "so.tilo.device#WindowsApps").expect("a resource id")
}

/// One program as the layer reports it, with `fields` over what it says.
fn reader_app() -> serde_json::Value {
    serde_json::json!({
        "id": "adobe-reader",
        "name": "Adobe Acrobat Reader",
        "publisher": "Adobe",
        "route": "wine",
        "recommended_route": "wine",
        "runner": "Wine 11.0",
        "capsule": "Adobe Reader",
        "setup_name": "Adobe Acrobat Reader",
        "source": "catalogue",
        "confidence": "verified",
        "reason": "A verified recipe opens Reader in Wine.",
        "status": "ready",
        "access": {"files_read_only": 1, "files_read_write": 0, "network": "none",
                   "microphone": false, "sound": true},
        "fallback_detail": "Reader sees only the folders listed above.",
        "launch_available": true,
        "launch_unavailable_reason": null,
        "portal_file_open_available": false,
        "portal_file_open_unavailable_reason": null,
        "route_override_available": true,
        "route_override_unavailable_reason": null,
        "can_retry_install": false,
        "can_escalate_install_to_vm": false,
        "can_remove_launcher": true,
        "can_vendor_uninstall": true,
        "can_delete_data": true,
        "can_publish_vm_candidate": false,
        "installation": "Pane 12 June 2026.",
        "vm_fallback_available": false,
        "vm_fallback_unavailable_reason": null
    })
}

fn studio_app() -> serde_json::Value {
    serde_json::json!({
        "id": "visual-studio",
        "name": "Visual Studio",
        "publisher": "Microsoft",
        "route": "vm",
        "recommended_route": "vm",
        "runner": "Windows 11",
        "capsule": "Shared Windows appliance",
        "setup_name": null,
        "source": "installed-in-windows",
        "confidence": "verified",
        "reason": "Visual Studio needs Windows services Wine can't provide.",
        "status": "running",
        "access": {"files_read_only": 0, "files_read_write": 0, "network": "none",
                   "microphone": false, "sound": true},
        "fallback_detail": "",
        "launch_available": true,
        "launch_unavailable_reason": null,
        "portal_file_open_available": false,
        "portal_file_open_unavailable_reason": "This app window can't receive files yet.",
        "route_override_available": false,
        "route_override_unavailable_reason": "Only an administrator can change how this app opens.",
        "can_retry_install": false,
        "can_escalate_install_to_vm": false,
        "can_remove_launcher": true,
        "can_vendor_uninstall": false,
        "can_delete_data": false,
        "can_publish_vm_candidate": false,
        "installation": "Pane 21 April 2026.",
        "vm_fallback_available": true,
        "vm_fallback_unavailable_reason": null
    })
}

fn appliance() -> serde_json::Value {
    serde_json::json!({
        "configured": true,
        "phase": "ready",
        "license": "digital",
        "power_state": "running",
        "published_applications": 1,
        "recovery_available": true,
        "temporary_connection": null,
        "shared_session": {"active": false, "grants": [], "contributors": []},
        "detail": null
    })
}

fn support() -> serde_json::Value {
    serde_json::json!({
        "available": true,
        "state": "ready",
        "summary": "Wine and Proton are installed.",
        "size": null,
        "problem": null,
        "pending": []
    })
}

/// A reading with `apps`, `appliance`, `pending_install` and `support` as given.
fn reading(
    apps: Vec<serde_json::Value>,
    appliance: serde_json::Value,
    pending: Option<serde_json::Value>,
    support: serde_json::Value,
) -> String {
    serde_json::json!({
        "serving": true,
        "summary": "The Windows appliance is ready.",
        "apps": apps,
        "appliance": appliance,
        "pending_install": pending,
        "support": support,
        "pending": [],
        "problem": null
    })
    .to_string()
}

/// A reading of the two sample programs.
fn full() -> String {
    reading(
        vec![reader_app(), studio_app()],
        appliance(),
        None,
        support(),
    )
}

fn installed(reading: String) -> Pane {
    Pane::install(&me(), SOURCE)
        .delegating()
        .reading(&windows(), &reading)
        .rooted("so.tilo.windows.Windows")
}

fn installed_full() -> Pane {
    installed(full())
}

/// The program `id` of the reading, as a view reads it.
fn program(app: &Pane, id: &str) -> Value {
    let runtime = app.host.runtime();
    let Value::List(programs) = runtime.read(&windows(), "apps").expect("the programs") else {
        panic!("the reading lists its programs");
    };
    programs
        .into_iter()
        .find(|value| match value {
            Value::Ref(held) => runtime.read(&held.target, "id").ok() == Some(Value::text(id)),
            _ => false,
        })
        .unwrap_or_else(|| panic!("the reading lists no {id}"))
}

/// The window with the program `chosen` picked.
fn shown_at(app: &Pane, chosen: &str) -> ViewInstance {
    app.host
        .runtime()
        .stage_view_state("so.tilo.windows", "chosen", chosen)
        .expect("the view state is declared");
    app.window()
}

fn words(node: &UiNode, into: &mut Vec<String>) {
    for prop in [
        "title",
        "value",
        "label",
        "secondary",
        "text",
        "meta",
        "badge",
        "message",
        "description",
    ] {
        if let Some(Value::Text(text)) = node.props.get(prop) {
            into.push(text.clone());
        }
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    out
}

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

/// The case `case` of the app's `OpenWith`, as the package declares it.
fn open_with(app: &Pane, case: &str) -> Value {
    let package = app
        .host
        .package("so.tilo.windows")
        .expect("the Windows package");
    let declared = package
        .meta
        .enums
        .iter()
        .find(|declared| declared.name == "OpenWith")
        .expect("OpenWith");
    let variant = declared
        .variants
        .iter()
        .find(|variant| variant.name == case)
        .expect("the case");
    Value::Enum(tilolang_runtime::value::EnumValue {
        ty: declared.id.clone(),
        variant: variant.id.clone(),
        fields: std::collections::BTreeMap::new(),
    })
}

fn installer() -> Value {
    Value::file(FileValue {
        path: "/home/ada/Downloads/setup.exe".to_owned(),
        ..FileValue::new(
            "setup.exe",
            "application/vnd.microsoft.portable-executable",
            4096,
            "h1",
        )
    })
}

#[test]
fn the_window_lists_every_program_and_the_appliance() {
    let app = installed_full();
    let window = app.window();
    for wanted in [
        "Adobe Acrobat Reader",
        "Adobe",
        "Visual Studio",
        "Running",
        "Wine",
        "Windows VM",
        "Windows appliance",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn an_empty_catalogue_says_so() {
    let app = installed(reading(Vec::new(), appliance(), None, support()));
    let window = app.window();
    assert!(contains(&window, "No Windows apps"));
}

#[test]
fn a_program_page_says_how_it_opens_what_it_reaches_and_where_it_was_learned() {
    let app = installed_full();
    let window = shown_at(&app, "adobe-reader");
    for wanted in [
        "Open with",
        "Files it can read",
        "One place",
        "Microphone",
        "About",
        "Tilo compatibility catalogue",
        "A verified recipe",
        "Wine 11.0",
        "Pane 12 June 2026.",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn a_failed_setup_leads_with_running_it_again() {
    let mut app = reader_app();
    app["status"] = serde_json::json!("failed");
    app["launch_available"] = serde_json::json!(false);
    app["can_retry_install"] = serde_json::json!(true);
    app["installation"] = serde_json::json!("The installer stopped before Reader was ready.");
    let installed = installed(reading(vec![app], appliance(), None, support()));
    let window = shown_at(&installed, "adobe-reader");
    for wanted in [
        "Its setup did not finish",
        "The installer stopped before Reader was ready.",
        "Run the setup again",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn the_waiting_setup_offers_both_places_to_go() {
    let app = installed(reading(
        vec![],
        appliance(),
        Some(serde_json::json!({
            "display_name": "Notepad++ 8.6.4",
            "detail": "A signed installer from Don Ho.",
            "recommended_route": "wine"
        })),
        support(),
    ));
    let window = shown_at(&app, "pending");
    for wanted in [
        "Install Notepad++ 8.6.4",
        "A signed installer from Don Ho.",
        "A private capsule",
        "Windows",
        "Recommended",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn the_appliance_page_states_its_power_licence_and_recovery() {
    let app = installed_full();
    let window = shown_at(&app, "appliance");
    for wanted in [
        "Windows is running.",
        "Activated with a digital license",
        "Published apps",
        "Restart Windows",
        "Suspend Windows",
        "Shut Windows down",
        "Open the recovery desktop",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn a_shut_down_appliance_offers_to_start_and_not_to_restart() {
    let mut state = appliance();
    state["power_state"] = serde_json::json!("shut-down");
    let app = installed(reading(vec![], state, None, support()));
    let window = shown_at(&app, "appliance");
    assert!(contains(&window, "Start Windows"), "{:?}", shown(&window));
    assert!(!contains(&window, "Restart Windows"));
    assert!(!contains(&window, "Shut Windows down"));
}

#[test]
fn a_suspended_appliance_offers_to_resume() {
    let mut state = appliance();
    state["power_state"] = serde_json::json!("suspended");
    let app = installed(reading(vec![], state, None, support()));
    let window = shown_at(&app, "appliance");
    assert!(contains(&window, "Resume Windows"), "{:?}", shown(&window));
    assert!(contains(&window, "Windows is suspended."));
}

#[test]
fn a_temporary_connection_says_what_it_is_for_and_can_be_ended() {
    let mut state = appliance();
    state["temporary_connection"] = serde_json::json!({
        "purpose": "activation",
        "expires_in_seconds": 300
    });
    let app = installed(reading(vec![], state, None, support()));
    let window = shown_at(&app, "appliance");
    assert!(contains(&window, "activation"), "{:?}", shown(&window));
    assert!(contains(&window, "5 minutes"));
    assert!(contains(&window, "End the connection"));
}

#[test]
fn choosing_a_route_writes_the_shared_choice_and_carries_it() {
    let app = installed_full();
    app.call(
        "set_route",
        vec![
            app.root_value(),
            Value::text("adobe-reader"),
            open_with(&app, "proton"),
        ],
    );
    let asked = app.sent.asked("set_route");
    assert!(
        asked.iter().any(|value| matches!(
            value,
            Value::Opt(Some(inner))
                if matches!(inner.as_ref(), Value::Enum(case) if case.variant == "Route.proton")
        )),
        "{asked:?}"
    );
}

#[test]
fn letting_the_catalogue_decide_carries_no_route() {
    let app = installed_full();
    app.call(
        "set_route",
        vec![
            app.root_value(),
            Value::text("adobe-reader"),
            open_with(&app, "wine"),
        ],
    );
    app.call(
        "use_recommendation",
        vec![app.root_value(), Value::text("adobe-reader")],
    );
    let asked = app.sent.last_asked("set_route");
    assert!(
        asked.iter().any(|value| matches!(value, Value::Opt(None))),
        "the last choice is no route: {asked:?}"
    );
}

#[test]
fn an_installer_opened_from_a_folder_is_installed() {
    let app = installed_full();
    app.call("installed", vec![app.root_value(), installer()]);
    let asked = app.sent.asked("install");
    assert!(
        asked.iter().any(|value| matches!(
            value,
            Value::Opt(Some(inner))
                if matches!(inner.as_ref(), Value::Text(path) if path == "/home/ada/Downloads/setup.exe")
        )),
        "{asked:?}"
    );
}

#[test]
fn a_windows_image_is_imported_and_a_document_opens_in_its_program() {
    // The appliance's program takes a file this computer chose, as its reading says.
    let mut studio = studio_app();
    studio["portal_file_open_available"] = serde_json::json!(true);
    studio["portal_file_open_unavailable_reason"] = serde_json::Value::Null;
    let app = installed(reading(vec![studio], appliance(), None, support()));
    app.call(
        "imported_iso",
        vec![Value::file(FileValue {
            path: "/home/ada/win11.iso".to_owned(),
            ..FileValue::new("win11.iso", "application/x-cd-image", 4096, "h2")
        })],
    );
    assert!(app.sent.names().iter().any(|name| name == "import-iso"));
    let studio = program(&app, "visual-studio");
    app.call("opened_file", vec![app.root_value(), studio, installer()]);
    let asked = app.sent.asked("open_file");
    assert!(
        asked
            .iter()
            .any(|value| matches!(value, Value::Text(app) if app == "visual-studio")),
        "{asked:?}"
    );
}

/// The appliance the person drives from search or the app's own menu, with no window open: each
/// of those actions reaches the computer's `windows-apps` reading.
#[test]
fn the_appliance_works_with_the_window_closed() {
    let app = installed_full();
    for (action, asked) in [
        ("recovery_desktop", "recovery"),
        ("start_windows", "start"),
        ("restart_windows", "restart"),
        ("suspend_windows", "suspend"),
        ("resume_windows", "resume"),
        ("shutdown_windows", "shutdown"),
        ("temporary_network", "temporary-network"),
        ("update_network", "update-network"),
        ("revoke_network", "revoke-network"),
    ] {
        app.call(action, Vec::new());
        assert!(
            app.sent.names().iter().any(|name| name == asked),
            "{action} asks `{asked}`: {:?}",
            app.sent.names()
        );
    }
}

#[test]
fn the_support_banner_offers_to_install_windows_support() {
    let mut support = support();
    support["state"] = serde_json::json!("needs-install");
    support["summary"] =
        serde_json::json!("Wine and Proton are not installed; installing takes 1.2 GB.");
    let app = installed(reading(vec![reader_app()], appliance(), None, support));
    let window = shown_at(&app, "adobe-reader");
    assert!(
        contains(&window, "Install Windows support"),
        "{:?}",
        shown(&window)
    );
    assert!(contains(&window, "1.2 GB"));
}

#[test]
fn a_program_of_the_appliance_says_what_its_window_cannot_receive() {
    let app = installed_full();
    let window = shown_at(&app, "visual-studio");
    assert!(contains(
        &window,
        "This app window can't receive files yet."
    ));
    assert!(contains(
        &window,
        "Only an administrator can change how this app opens."
    ));
}

#[test]
fn the_operations_an_installation_allows_are_the_ones_shown() {
    let mut app = reader_app();
    app["can_publish_vm_candidate"] = serde_json::json!(true);
    app["can_vendor_uninstall"] = serde_json::json!(true);
    let first = installed(reading(vec![app], appliance(), None, support()));
    let window = shown_at(&first, "adobe-reader");
    for wanted in ["Add to Windows Apps", "Uninstall", "Delete its data"] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
    let mut quiet = reader_app();
    quiet["can_vendor_uninstall"] = serde_json::json!(false);
    quiet["can_delete_data"] = serde_json::json!(false);
    let quiet_app = installed(reading(vec![quiet], appliance(), None, support()));
    let window = shown_at(&quiet_app, "adobe-reader");
    assert!(!contains(&window, "Uninstall"));
    assert!(!contains(&window, "Delete its data"));
}

#[test]
fn the_setups_programs_are_named_before_their_data_goes() {
    let mut suite = reader_app();
    suite["id"] = serde_json::json!("putty");
    suite["name"] = serde_json::json!("PuTTY");
    suite["capsule"] = serde_json::json!("wine-putty");
    suite["setup_name"] = serde_json::json!("PuTTY 0.85 (64-bit)");
    let mut second = suite.clone();
    second["id"] = serde_json::json!("psftp");
    second["name"] = serde_json::json!("PSFTP");
    let installed = installed(reading(vec![suite, second], appliance(), None, support()));
    installed
        .host
        .runtime()
        .stage_view_state("so.tilo.windows", "confirm_kind", "delete")
        .expect("the confirmation is view state");
    installed
        .host
        .runtime()
        .stage_view_state("so.tilo.windows", "confirm_target", "wine-putty")
        .expect("its target is view state");
    let window = shown_at(&installed, "putty");
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text.contains("Delete PuTTY 0.85 (64-bit)'s data?")),
        "{words:?}"
    );
    assert!(
        words.iter().any(|text| text.contains("PuTTY, PSFTP")),
        "both programs of the setup are named: {words:?}"
    );
}

#[test]
fn a_program_that_ended_says_how() {
    let mut ended = serde_json::json!({
        "serving": true,
        "summary": "The Windows appliance is ready.",
        "apps": [reader_app(), studio_app()],
        "appliance": appliance(),
        "pending_install": null,
        "support": support(),
        "ended": {"program": "adobe-reader", "name": "Adobe Acrobat Reader", "success": false,
                  "exit_code": 3, "error": null, "said": "The capsule's runner is not installed."},
        "pending": [],
        "problem": null
    });
    let app = installed(ended.to_string());
    let window = app.window();
    assert!(
        contains(&window, "Adobe Acrobat Reader stopped"),
        "{:?}",
        shown(&window)
    );
    assert!(contains(
        &window,
        "It stopped with code 3: The capsule's runner is not installed."
    ));
    ended["ended"] = serde_json::json!({
        "program": "adobe-reader", "name": "Adobe Acrobat Reader", "success": false,
        "exit_code": null, "error": "The capsule's runner is not installed.", "said": null
    });
    let app = installed(ended.to_string());
    let window = app.window();
    assert!(contains(&window, "Adobe Acrobat Reader could not open"));
    assert!(contains(&window, "The capsule's runner is not installed."));
}

#[test]
fn what_the_service_reports_as_wrong_is_said() {
    let mut state = serde_json::json!({
        "serving": true,
        "summary": "The Windows appliance is ready.",
        "apps": [reader_app()],
        "appliance": appliance(),
        "pending_install": null,
        "support": support(),
        "pending": [],
        "problem": "The compatibility service did not answer."
    });
    state["problem"] = serde_json::json!("The compatibility service did not answer.");
    let app = installed(state.to_string());
    let window = app.window();
    assert!(contains(
        &window,
        "The compatibility service did not answer."
    ));
}

/// The window as the system opens it: an instance of the app over this computer's reading, so
/// what search asks of the app (its `open`, told a program's anchor) reaches the window.
struct Session;

impl tilolang_tilo::instance::Platform for Session {
    fn load(&self, host: &Host) -> u64 {
        let metas = tilolang_tilo::platform_apps::named_platform_packages(SOURCE)
            .expect("the platform's packages read");
        for meta in &metas {
            host.runtime()
                .load_foreign(meta.clone())
                .expect("the device is loaded");
        }
        let compiled = tilolang::compile_package(SOURCE, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        host.load_compiled(&compiled).expect("the app loads");
        host.runtime()
            .set_reading(&windows(), &full())
            .expect("the Windows programs read");
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-22T12:00:00Z".to_owned()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        std::sync::Arc::new(|| {})
    }
}

struct Named;

impl tilolang_tilo::runtime::NameHost for Named {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// The Windows Apps window, opened as the session opens it.
fn open_session() -> tilolang_tilo::instance::Instance {
    use tilolang_tilo::instance::{Config, Mode};
    let (app_end, _window) = tilo_render_protocol::patch::local::pair();
    let mut window = tilolang_tilo::instance::Instance::new(
        Config {
            app: "so.tilo.windows".to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Session),
        Rc::new(Named),
        Box::new(app_end),
    );
    window.sync();
    window
}

/// Whether the window says `wanted` anywhere in its tree.
fn says(window: &tilolang_tilo::instance::Instance, wanted: &str) -> bool {
    let Some(tree) = window.root_tree() else {
        return false;
    };
    let mut texts = Vec::new();
    words(&tree, &mut texts);
    texts.iter().any(|text| text.contains(wanted))
}

/// A program found in search opens the window on that program's page: the app's `open`, told
/// the program the anchor names, shows it as choosing its row does.
#[test]
fn a_search_hit_on_a_program_opens_the_window_at_its_page() {
    use tilolang_tilo::OpenEffect;

    let mut window = open_session();
    assert!(
        says(&window, "Choose a program on the left"),
        "nothing is chosen yet"
    );
    window.open_requests().borrow_mut().push(OpenEffect::Open {
        anchor: "windows-apps/visual-studio".to_owned(),
    });
    window.update();
    window.sync();
    assert!(
        says(
            &window,
            "Visual Studio needs Windows services Wine can't provide."
        ),
        "the program search named is the page the window shows"
    );
    window.open_requests().borrow_mut().push(OpenEffect::Open {
        anchor: "windows-apps/adobe-reader".to_owned(),
    });
    window.update();
    window.sync();
    assert!(
        says(&window, "A verified recipe opens Reader in Wine."),
        "another hit moves the window to its program"
    );
    assert!(!says(&window, "Choose a program on the left"));
}

/// The plan a hit on a program resolves to: the program, as the reading names it, in this
/// window. One no list of the reading holds is nothing to open.
#[test]
fn a_program_of_the_reading_is_the_record_the_window_opens() {
    let app = installed_full();
    let plan = app
        .host
        .open_plan("so.tilo.windows", "windows-apps/adobe-reader", None)
        .expect("a program's page");
    assert_eq!(plan.app, "so.tilo.windows");
    let target = plan.target.expect("the program");
    assert_eq!(
        target.id_string(),
        "adobe-reader",
        "a reading names a record by its own id"
    );
    assert_eq!(target.ty, "so.tilo.device#WindowsApp");
    assert!(
        app.host
            .open_plan("so.tilo.windows", "windows-apps/not-a-program", None)
            .is_err(),
        "a program the reading does not list names nothing"
    );
}
