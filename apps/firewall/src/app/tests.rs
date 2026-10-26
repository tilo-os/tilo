use super::*;
use automerge::AutoCommit;
use ui::headless::Framed;
use ui::prelude::TestApp;
use ui::widget::SidebarPlacement;
use vello::kurbo::Rect;

use crate::resource::FirewallShell;

/// The bare widget showing the sample ruleset, framed in a window of
/// `w`×`h`: for its layout and pointer input.
fn framed(w: f64, h: f64) -> Framed<FirewallApp> {
    framed_app(FirewallApp::new_demo(), w, h)
}

fn framed_app(app: FirewallApp, w: f64, h: f64) -> Framed<FirewallApp> {
    let mut state = ui::State::new();
    assert!(state.adopt_doc(AutoCommit::new()));
    Framed::new(state, app, ui::headless::viewport((w, h)), Theme::dark())
}

/// The app hosted on its resource, holding the sample ruleset: every
/// control below reaches the document through the dispatcher.
fn hosted() -> TestApp<FirewallShell> {
    let mut app = TestApp::<FirewallShell>::new().unwrap();
    app.invoke(Firewall::import_of(&crate::sample::sample_config()))
        .unwrap();
    app
}

/// The rect of the widget whose target id ends with `suffix`.
fn find_target(harness: &ui::headless::FrameHarness, suffix: &str) -> Rect {
    let targets = harness.targets();
    let found = targets
        .iter()
        .find(|(id, _)| id.ends_with(suffix))
        .map(|(_, rect)| *rect);
    found.unwrap_or_else(|| panic!("no widget {suffix} in {:?}", targets.keys()))
}

fn target(app: &Framed<FirewallApp>, suffix: &str) -> Rect {
    find_target(Framed::harness(app), suffix)
}

fn click(app: &mut Framed<FirewallApp>, rect: Rect) {
    let c = rect.center();
    Framed::harness_mut(app).click_at(c.x, c.y);
}

/// Click the hosted control whose target id ends with `suffix`, where a
/// person sees it, and run the frame that follows.
fn press(app: &mut TestApp<FirewallShell>, suffix: &str) {
    let c = find_target(app.harness(), suffix).center();
    app.harness().click_at(c.x, c.y);
    app.settle();
}

/// Press the window bar's slot `id`, as the desktop hands the press over,
/// and run the frame that follows.
fn press_bar(app: &mut TestApp<FirewallShell>, id: &str) {
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.to_owned() });
    app.settle();
}

/// Open a zone other than the active one from the zone list: the first
/// row whose page offers to make it active.
fn open_another_zone(app: &mut TestApp<FirewallShell>) {
    let list = find_target(app.harness(), "NavList");
    let row = f64::from(Theme::dark().tokens.metrics.nav_row_height);
    for i in 0..20 {
        app.harness()
            .click_at(list.x0 + 60.0, list.y0 + row * (f64::from(i) + 0.5));
        app.settle();
        if app
            .harness()
            .targets()
            .keys()
            .any(|id| id.ends_with("firewall-make-active"))
        {
            return;
        }
    }
    panic!("no zone in the list other than the active one opened");
}

fn public_rules(app: &TestApp<FirewallShell>) -> Vec<Rule> {
    app.state()
        .config()
        .zone(ZoneKind::Public)
        .map(|zone| zone.rules.clone())
        .unwrap_or_default()
}

/// Every control the captures show is reachable through the action door.
#[test]
fn populated_firewall_passes_native_certification() {
    for scenario in [
        None,
        Some("services"),
        Some("add-rule"),
        Some("confirmation"),
        Some("narrow"),
        Some("service-error"),
    ] {
        let mut app =
            TestApp::<FirewallShell>::capture(scenario).expect("the app opens headlessly");
        let audit = app.harness().native_app_audit();
        assert!(
            audit.issues.is_empty() && audit.accessibility.issues.is_empty(),
            "{scenario:?}: native={:?}, accessibility={:?}",
            audit.issues,
            audit.accessibility.issues
        );
    }
}

/// Nothing shown is fabricated, an unavailable firewall is one empty state
/// with one action, and a failed recovery changes nothing and says why.
#[test]
fn backend_availability_governs_what_is_shown_and_announced() {
    // --- phase: nothing_shown_is_fabricated ---
    {
        // Before the resource presents a configuration the surface shows
        // none, never the fabricated sample.
        let live = FirewallApp::new();
        assert!(!live.available);
        assert!(live.config.zones.is_empty());
    }

    // --- phase: failed_recovery_changes_nothing_and_says_why ---
    {
        let mut app = FirewallApp::new();
        let failed = app.finish_retry(Err(
            "Firewalld is still not reachable. Start the service, then try again.".into(),
        ));
        assert!(failed.is_err());
        assert!(!app.available);
        assert!(app.config.zones.is_empty());
        assert_eq!(
            app.unavailable_detail(),
            Some("Firewalld is still not reachable. Start the service, then try again.")
        );
        let semantics = app.accessibility().expect("a window");
        assert!(semantics.children.iter().any(|child| {
            child.name.as_deref() == app.unavailable_detail() && child.live.is_some()
        }));
    }

    // --- phase: the_unavailable_window_is_one_empty_state_with_one_action ---
    {
        let app = framed_app(FirewallApp::new(), 1280.0, 800.0);
        assert_eq!(app.shell.geometry().sidebar, SidebarPlacement::None);
        let retry = target(&app, "EmptyState");
        assert!(retry.width() > 0.0);
    }

    // --- phase: a_capture_shows_the_sample_and_never_the_host ---
    {
        let app = TestApp::<FirewallShell>::capture(None).unwrap();
        assert_eq!(
            app.state().config().zones.len(),
            crate::sample::sample_config().zones.len()
        );
    }
}

/// The zones dock beside the page while the page keeps its minimum width
/// beside them, and become a screen of their own below that.
#[test]
fn the_zones_dock_while_they_fit_and_become_a_screen_when_narrow() {
    for width in [1280.0, 640.0] {
        let docked = framed(width, 800.0);
        assert_eq!(
            docked.shell.geometry().sidebar,
            SidebarPlacement::Docked,
            "at {width} px"
        );
        assert!(docked.shell.is_sidebar_open(), "at {width} px");
    }

    let narrow = framed(480.0, 600.0);
    assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Screen);
}

#[test]
fn choosing_a_zone_in_the_list_opens_its_page() {
    let mut app = framed(1280.0, 800.0);
    let list = target(&app, "NavList");
    // The active zone (public) heads the list; the other zones follow it.
    let row = f64::from(Theme::dark().tokens.metrics.nav_row_height);
    let rows: Vec<f64> = (0..20)
        .map(|i| list.y0 + row * (f64::from(i) + 0.5))
        .collect();
    let mut opened = Vec::new();
    for y in rows {
        Framed::harness_mut(&mut app).click_at(list.x0 + 60.0, y);
        if !opened.contains(&app.selected_zone) {
            opened.push(app.selected_zone);
        }
    }
    assert!(opened.contains(&ZoneKind::Drop), "{opened:?}");
    assert!(opened.contains(&ZoneKind::Trusted), "{opened:?}");
}

/// Choosing a zone, a tab or a rule changes what the window shows and never the document.
#[test]
fn presentation_changes_do_not_create_document_versions() {
    // --- phase: intents_change_only_the_view ---
    {
        let mut app = FirewallApp::new_demo();
        for intent in [
            Intent::SelectZone(ZoneKind::Home),
            Intent::SetTab(Tab::Services),
            Intent::SetMatchService(false),
            Intent::SetProto(Proto::Udp),
        ] {
            app.intents.push(intent);
        }
        app.drain();
        assert_eq!(app.selected_zone, ZoneKind::Home);
        assert_eq!(app.tab, Tab::Services);
        assert!(!app.draft.service);
        assert_eq!(app.draft.proto, Proto::Udp);
    }

    // --- phase: hosted_selection_creates_no_version ---
    {
        let mut app = hosted();
        let versions = app.versions().len();
        open_another_zone(&mut app);
        press_bar(&mut app, Tab::Services.view_id());
        assert_eq!(app.versions().len(), versions);
    }
}

/// Rules are added from the draft only when valid, and the field says what is wrong.
#[test]
fn the_add_rule_form_validates_each_field() {
    let mut app = FirewallApp::new_demo();
    app.adding = true;
    app.draft.service = false;
    app.draft.port.set("70000".into());
    app.draft.source.set("10.0.0".into());
    app.intents.push(Intent::AddRule);
    app.drain();
    assert_eq!(
        app.draft.port_error.as_deref(),
        Some("Use a port from 1 to 65535.")
    );
    assert!(app.draft.source_error.is_some());
    assert!(app.adding, "an invalid draft stays open");
    assert!(app.selected_rule.is_none());

    app.draft.port.set("9000".into());
    app.draft.source.set(String::new());
    app.intents.push(Intent::AddRule);
    app.drain();
    assert!(app.draft.port_error.is_none() && app.draft.source_error.is_none());
    assert!(app.selected_rule.is_some(), "the new rule is selected");
    assert!(!app.adding, "the dialog closes once added");
    assert!(app.draft.port().is_empty(), "the draft clears once added");
}

/// Deleting asks first; only the confirmation removes the rule, and undo
/// brings it back.
#[test]
fn deleting_a_rule_asks_first() {
    // --- phase: cancel_keeps_the_rule ---
    {
        let mut app = FirewallApp::new_demo();
        let id = app.config.zone(ZoneKind::Public).unwrap().rules[0].id;
        app.intents.push(Intent::AskDelete(id));
        app.drain();
        assert_eq!(app.pending_delete, Some(id));
        app.intents.push(Intent::CancelDelete);
        app.drain();
        assert_eq!(app.pending_delete, None);
    }

    // --- phase: confirmed_delete_reaches_the_document ---
    {
        let mut app = hosted();
        let rule = public_rules(&app)[0].clone();
        press(&mut app, &format!("firewall-rule-delete-{}", rule.id));
        assert!(
            public_rules(&app).iter().any(|r| r.id == rule.id),
            "the first press only asks for confirmation"
        );
        // Return is the safe answer in a destructive confirmation.
        app.harness().key("Enter");
        app.settle();
        assert!(
            public_rules(&app).iter().any(|r| r.id == rule.id),
            "Enter cancels the delete"
        );
        press(&mut app, &format!("firewall-rule-delete-{}", rule.id));
        press(&mut app, "firewall-delete-dialog.primary");
        assert!(!public_rules(&app).iter().any(|r| r.id == rule.id));

        assert!(app.undo());
        assert!(public_rules(&app).iter().any(|r| r.id == rule.id));
    }
}

/// Rule and service switches write the document; the window bar's switches toggle panic mode and the firewall.
#[test]
fn switches_round_trip_through_the_document() {
    let mut app = hosted();
    let rule = public_rules(&app)[0].clone();
    press(&mut app, &format!("firewall-rule-enabled-{}", rule.id));
    assert_eq!(
        public_rules(&app)[0].enabled,
        !rule.enabled,
        "the rule's switch writes the document"
    );

    press_bar(&mut app, Tab::Services.view_id());
    press(&mut app, "firewall-service-ssh");
    assert!(
        !app.state()
            .config()
            .zone(ZoneKind::Public)
            .unwrap()
            .has_service("ssh"),
        "ssh is on in the sample and the switch turned it off"
    );

    // Panic mode and stopping the firewall cut traffic: each waits for the
    // person to confirm what the host asks.
    assert!(!app.state().config().panic_mode);
    press_bar(&mut app, view::PANIC_MODE);
    assert!(
        !app.state().config().panic_mode,
        "panic mode waits for consent"
    );
    confirm(&mut app);
    assert!(app.state().config().panic_mode);

    press_bar(&mut app, view::FIREWALL_ON);
    assert!(app.state().config().enabled, "stopping waits for consent");
    confirm(&mut app);
    assert!(!app.state().config().enabled);
}

/// Confirm what the host asks, as the person does with its verb button.
fn confirm(app: &mut TestApp<FirewallShell>) {
    assert!(app.confirm_asked().is_some(), "the host asks");
}

#[test]
fn making_a_zone_active_changes_the_default() {
    let mut app = hosted();
    open_another_zone(&mut app);
    let before = app.state().config().default_zone;
    press(&mut app, "firewall-make-active");
    assert_ne!(app.state().config().default_zone, before);
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("is now the active zone"), "{frame}");
}

#[test]
fn the_row_delete_button_opens_the_confirmation() {
    let mut app = framed(1280.0, 800.0);
    let first = app.config.zone(ZoneKind::Public).unwrap().rules[0].id;
    let button = target(&app, &format!("firewall-rule-delete-{first}"));
    click(&mut app, button);
    assert_eq!(app.pending_delete, Some(first));
}

/// Applying now runs through the same door as the palette and reports how
/// it ended; a unit test plans the same commands without touching the host.
#[test]
fn apply_now_reports_the_checked_changes() {
    let mut app = hosted();
    app.effect(&FirewallEffect::ApplyConfig).unwrap();
    app.settle();
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("changes"), "{frame}");
}

/// The rules are one list, allows first; pressing a header sorts by that
/// column and pressing it again flips the order.
#[test]
fn the_rules_are_one_list_sorted_from_its_headers() {
    let mut app = framed(1280.0, 800.0);
    let rules = app.config.zone(ZoneKind::Public).unwrap().rules.clone();
    let rows = |app: &Framed<FirewallApp>| {
        let mut rows: Vec<(f64, Action)> = rules
            .iter()
            .map(|rule| {
                let delete = target(app, &format!("firewall-rule-delete-{}", rule.id));
                (delete.y0, rule.action)
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        rows.into_iter()
            .map(|(_, action)| action)
            .collect::<Vec<_>>()
    };
    let order = rows(&app);
    assert!(order.contains(&Action::Allow) && order.contains(&Action::Deny));
    assert!(order.is_sorted_by_key(|action| *action == Action::Deny));

    let table = target(&app, "firewall-rules");
    let header = Point::new(table.x0 + 24.0, table.y0 + 12.0);
    Framed::harness_mut(&mut app).click_at(header.x, header.y);
    Framed::harness_mut(&mut app).poll_frame().unwrap();
    assert_eq!(
        app.rule_sort.with(Clone::clone),
        Some(TableSort {
            key: "action".into(),
            ascending: false
        })
    );
    let flipped = rows(&app);
    assert!(flipped.is_sorted_by_key(|action| *action == Action::Allow));
}

/// The firewall's controls are the window bar's slots: the open zone names
/// the window, the switch, Apply and Add rule are its actions with panic
/// mode in its menu, and Rules and Services are its views. The window draws
/// no header row, tabs or switches of its own.
#[test]
fn the_firewalls_controls_are_in_the_window_bar() {
    let mut app = hosted();
    app.harness().present_in_window_bar();
    app.settle();
    let bar = ui::window_toolbar::declared().expect("the window declares its slots");
    let zone = app.state().config().default_zone;
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some(zone.title())
    );
    let actions: Vec<(&str, bool, bool)> = bar
        .actions
        .iter()
        .map(|action| (action.id.as_str(), action.on, action.menu))
        .collect();
    assert_eq!(
        actions,
        [
            (view::FIREWALL_ON, true, false),
            (view::APPLY, false, false),
            (view::ADD_RULE, false, false),
            (view::PANIC_MODE, false, true),
        ]
    );
    let views = bar.views.expect("Rules and Services are the bar's views");
    assert_eq!(views.chosen, Tab::Rules.view_id());
    let targets = app.harness().targets();
    for drawn in [
        "firewall-tabs",
        "firewall-enabled",
        "firewall-panic",
        "firewall-apply",
    ] {
        assert!(
            !targets.keys().any(|id| id.ends_with(drawn)),
            "the window draws no {drawn} of its own"
        );
    }

    press_bar(&mut app, view::ADD_RULE);
    assert!(
        app.harness()
            .targets()
            .keys()
            .any(|id| id.ends_with("firewall-add-rule-dialog")),
        "the bar's Add rule opens the dialog"
    );
    press_bar(&mut app, Tab::Services.view_id());
    let bar = ui::window_toolbar::declared().expect("the window's slots");
    assert!(
        bar.actions.iter().all(|action| action.id != view::ADD_RULE),
        "Add rule is the rules' command"
    );
}
