use super::*;
use ui::headless::Framed;
use ui::widget::SidebarPlacement;

impl FirewallApp {
    fn start_op(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        Widget::start_operation(self, op, doc, path)
    }

    fn poll_op(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        Widget::poll_resource_operation(self, invocation, cancel, doc, path)
    }
}

fn seeded() -> (FirewallApp, AutoCommit) {
    let mut app = FirewallApp::new_demo();
    let mut doc = AutoCommit::new();
    Widget::sync(&mut app, &mut doc, &automerge::ROOT);
    (app, doc)
}

fn framed((app, doc): (FirewallApp, AutoCommit), w: f64, h: f64) -> Framed<FirewallApp> {
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc));
    Framed::new(state, app, ui::headless::viewport((w, h)), Theme::dark())
}

fn doc_of(app: &Framed<FirewallApp>) -> &AutoCommit {
    &Framed::harness(app).state().doc
}

/// The rect of the widget whose target id ends with `suffix`.
fn target(app: &Framed<FirewallApp>, suffix: &str) -> Rect {
    let targets = Framed::harness(app).targets();
    let found = targets
        .iter()
        .find(|(id, _)| id.ends_with(suffix))
        .map(|(_, rect)| *rect);
    found.unwrap_or_else(|| panic!("no widget {suffix} in {:?}", targets.keys()))
}

fn click(app: &mut Framed<FirewallApp>, rect: Rect) {
    let c = rect.center();
    Framed::harness_mut(app).click_at(c.x, c.y);
}

fn public_rules(doc: &AutoCommit) -> Vec<Rule> {
    FirewallDoc::load(doc)
        .and_then(|config| config.zone(ZoneKind::Public).map(|zone| zone.rules.clone()))
        .unwrap_or_default()
}

/// A demo ruleset is published through the firewall action, not a raw store.
#[test]
fn a_demo_firewall_is_published_through_the_action() {
    let (app, doc) = seeded();
    assert!(!app.actions.is_hosted());
    let recorded = app
        .last_dispatched()
        .expect("the imported ruleset is submitted through the action sink");
    assert_eq!(
        recorded.type_id.as_str(),
        crate::actions::FIREWALL_ACTION_TYPE
    );
    assert!(FirewallDoc::load(&doc).is_some_and(|config| !config.zones.is_empty()));
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
        let mut app = ui::framework::testing::HeadlessApp::<crate::FirewallShell>::new()
            .expect("the app opens headlessly");
        app.stage(scenario).expect("the scenario stages");
        let audit = app.harness().native_app_audit();
        // The window's History control binds the host's History door, which
        // the window publishes and the headless harness's resource tree does
        // not carry; every other control must resolve.
        let issues: Vec<_> = audit
            .issues
            .iter()
            .filter(|issue| {
                !(issue.control == "nav-shell-history" && issue.code == "stale-action-binding")
            })
            .collect();
        assert!(
            issues.is_empty() && audit.accessibility.issues.is_empty(),
            "{scenario:?}: native={issues:?}, accessibility={:?}",
            audit.accessibility.issues
        );
    }
}

/// Live, unavailable, failed-recovery and demo builds write only honest state to the document and announce backend status.
#[test]
fn backend_availability_governs_what_is_written_and_announced() {
    // --- phase: live_new_does_not_fabricate_sample_config ---
    {
        let live = FirewallApp::new();
        assert_ne!(
            live.config,
            crate::sample::sample_config(),
            "live app must not start on the fabricated sample firewall"
        );
    }

    // --- phase: unavailable_firewall_writes_nothing_and_offers_retry ---
    {
        let mut app = FirewallApp::from_parts(FirewallConfig::empty(), false);
        let mut doc = AutoCommit::new();
        Widget::sync(&mut app, &mut doc, &automerge::ROOT);
        assert!(!app.available);
        assert!(
            FirewallDoc::load(&doc).is_none(),
            "the unavailable state must leave the document empty, not seed a config"
        );
        let resources = ui::resources::resources(&app, &doc);
        assert_eq!(resources[0].kind, "firewall.recovery");
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|action| action.id == "retry")
        );

        app.recover_from_probe(None, &mut doc);
        assert!(!app.available);
        assert!(FirewallDoc::load(&doc).is_none());
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
        let mut app = framed(
            (
                FirewallApp::from_parts(FirewallConfig::empty(), false),
                AutoCommit::new(),
            ),
            1280.0,
            800.0,
        );
        assert_eq!(app.shell.geometry().sidebar, SidebarPlacement::None);
        let retry = target(&app, "EmptyState");
        assert!(retry.width() > 0.0);
        app.intents.push(Intent::Retry);
        Framed::harness_mut(&mut app).settle_for_capture(ui::time::Duration::from_secs(1));
        assert!(!app.available, "the test host has no firewalld");
    }

    // --- phase: demo_build_publishes_sample_for_shots ---
    {
        let (_, doc) = seeded();
        let stored = FirewallDoc::load(&doc).expect("demo build seeds the document");
        assert_eq!(
            stored.zones.len(),
            crate::sample::sample_config().zones.len()
        );
    }
}

/// The zones dock beside the page when wide and become a drawer when narrow.
#[test]
fn the_zones_dock_when_wide_and_collapse_to_a_drawer_when_narrow() {
    let wide = framed(seeded(), 1280.0, 800.0);
    assert_eq!(wide.shell.geometry().sidebar, SidebarPlacement::Docked);
    assert!(wide.shell.is_sidebar_open());

    let narrow = framed(seeded(), 640.0, 600.0);
    assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Drawer);
    assert!(!narrow.shell.is_sidebar_open());
}

/// Choosing a zone, a tab or a rule changes what the window shows and never the document.
#[test]
fn presentation_changes_do_not_create_document_heads() {
    let (mut app, mut doc) = seeded();
    let heads = doc.get_heads();
    for intent in [
        Intent::SelectZone(ZoneKind::Home),
        Intent::SetTab(Tab::Services),
        Intent::SetMatchService(false),
        Intent::SetProto(Proto::Udp),
    ] {
        app.intents.push(intent);
    }
    app.drain(&mut doc);
    assert_eq!(app.selected_zone, ZoneKind::Home);
    assert_eq!(app.tab, Tab::Services);
    assert!(!app.draft.service);
    assert_eq!(doc.get_heads(), heads);
}

/// The zone list selects a zone and the page follows it.
#[test]
fn choosing_a_zone_in_the_list_opens_its_page() {
    let mut app = framed(seeded(), 1280.0, 800.0);
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

/// Rules are added from the draft only when valid, and the field says what is wrong.
#[test]
fn the_add_rule_form_validates_each_field() {
    let (mut app, mut doc) = seeded();
    let before = public_rules(&doc).len();

    app.draft.service = false;
    app.draft.port.set("70000".into());
    app.draft.source.set("10.0.0".into());
    app.intents.push(Intent::AddRule);
    app.drain(&mut doc);
    assert_eq!(public_rules(&doc).len(), before);
    assert_eq!(
        app.draft.port_error.as_deref(),
        Some("Use a port from 1 to 65535.")
    );
    assert!(app.draft.source_error.is_some());

    app.draft.port.set("9000".into());
    app.draft.source.set(String::new());
    app.intents.push(Intent::AddRule);
    app.drain(&mut doc);
    let rules = public_rules(&doc);
    assert_eq!(rules.len(), before + 1);
    let added = rules.last().unwrap();
    assert_eq!(
        added.matcher,
        Matcher::Port {
            port: "9000".into(),
            proto: Proto::Tcp
        }
    );
    assert_eq!(app.selected_rule, Some(added.id));
    assert!(app.draft.port_error.is_none() && app.draft.source_error.is_none());
    assert!(app.draft.port().is_empty(), "the draft clears once added");

    app.draft.service = true;
    app.draft.service_id.set("wireguard".into());
    app.intents.push(Intent::AddRule);
    app.drain(&mut doc);
    assert_eq!(
        public_rules(&doc).last().unwrap().matcher,
        Matcher::Service("wireguard".into())
    );
}

/// Deleting asks first; only the confirmation removes the rule, and it is the palette's action.
#[test]
fn deleting_a_rule_asks_first_and_dispatches_the_palette_action() {
    use tilo_actions::ActionInput;

    let (mut app, mut doc) = seeded();
    let rule = public_rules(&doc)[0].clone();

    app.intents.push(Intent::AskDelete(rule.id));
    app.drain(&mut doc);
    assert_eq!(app.pending_delete, Some(rule.id));
    assert!(public_rules(&doc).iter().any(|r| r.id == rule.id));

    app.intents.push(Intent::CancelDelete);
    app.drain(&mut doc);
    assert_eq!(app.pending_delete, None);
    assert!(public_rules(&doc).iter().any(|r| r.id == rule.id));

    let resources = ui::resources::resources(&app, &doc);
    let spec = resources[0]
        .children
        .iter()
        .find(|z| z.id.as_str() == "firewall.zone:public")
        .and_then(|z| {
            z.children
                .iter()
                .find(|r| r.id.as_str() == format!("firewall.rule:{}", rule.id))
        })
        .and_then(|r| r.actions.iter().find(|act| act.id == "delete"))
        .map(|act| match &act.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("delete is an immediate action")
            }
        })
        .expect("a delete spec for the rule");

    app.intents.push(Intent::AskDelete(rule.id));
    app.intents.push(Intent::ConfirmDelete);
    app.drain(&mut doc);
    assert!(!public_rules(&doc).iter().any(|r| r.id == rule.id));
    assert_eq!(app.last_dispatched().cloned(), Some(spec));
    assert_eq!(app.pending_delete, None);
}

/// The delete button in a row opens the confirmation dialog.
#[test]
fn the_row_delete_button_opens_the_confirmation() {
    let mut app = framed(seeded(), 1280.0, 800.0);
    let first = public_rules(doc_of(&app))[0].id;
    let button = target(&app, &format!("firewall-rule-delete-{first}"));
    click(&mut app, button);
    assert_eq!(app.pending_delete, Some(first));
}

/// Rule and service switches write the document; the header switches toggle panic mode and the firewall.
#[test]
fn switches_round_trip_through_the_document() {
    let (mut app, mut doc) = seeded();
    // Building the page binds a switch per rule and per service.
    app.tab = Tab::Services;
    app.changed = true;
    app.refresh_view();
    let ssh = app.switches.state(SwitchKey::Service("ssh"), true);
    ssh.set(false);
    app.drain(&mut doc);
    let config = FirewallDoc::load(&doc).unwrap();
    assert!(!config.zone(ZoneKind::Public).unwrap().has_service("ssh"));

    let rule = public_rules(&doc)[0].clone();
    let state = app.switches.state(SwitchKey::Rule(rule.id), rule.enabled);
    state.set(!rule.enabled);
    app.drain(&mut doc);
    assert_eq!(
        public_rules(&doc)[0].enabled,
        !rule.enabled,
        "the rule's switch writes the document"
    );

    assert!(!app.config.panic_mode);
    app.panic_switch.set(true);
    app.drain(&mut doc);
    assert!(FirewallDoc::load(&doc).unwrap().panic_mode);

    app.enabled_switch.set(false);
    app.drain(&mut doc);
    assert!(!FirewallDoc::load(&doc).unwrap().enabled);
}

/// Making a zone active changes the document's default zone.
#[test]
fn making_a_zone_active_changes_the_default() {
    let (mut app, mut doc) = seeded();
    app.intents.push(Intent::SelectZone(ZoneKind::Home));
    app.intents.push(Intent::MakeActive);
    app.drain(&mut doc);
    assert_eq!(app.config.default_zone, ZoneKind::Home);
    assert_eq!(
        FirewallDoc::load(&doc).unwrap().default_zone,
        ZoneKind::Home
    );
    assert_eq!(
        app.notice.as_ref().map(|n| n.text.as_str()),
        Some("Home is now the active zone")
    );
}

/// An exported firewall effect is accepted then completes after sync; cancellation before sync prevents it.
#[test]
fn exported_effects_complete_only_after_sync() {
    // --- phase: exported_effect_is_accepted_then_completes_after_sync ---
    {
        let (mut app, mut doc) = seeded();
        let invocation = tilo_actions::InvocationId::new();
        let started = app
            .start_op(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    "firewall.apply",
                    &serde_json::to_vec(&"").unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            )
            .unwrap();
        assert_eq!(
            started,
            Some(ui::widget::ResourceOperationStart::Accepted),
            "queue admission must not be reported as success"
        );
        assert!(
            app.poll_op(invocation, false, &doc, &automerge::ROOT)
                .is_none()
        );
        Widget::sync(&mut app, &mut doc, &automerge::ROOT);
        assert!(matches!(
            app.poll_op(invocation, false, &doc, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    // --- phase: cancellation_before_sync_prevents_firewall_effect ---
    {
        let (mut app, mut doc) = seeded();
        let invocation = tilo_actions::InvocationId::new();
        app.start_op(
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                "firewall.apply",
                &serde_json::to_vec(&"").unwrap(),
            ),
            &mut doc,
            &automerge::ROOT,
        )
        .unwrap();
        assert!(matches!(
            app.poll_op(invocation, true, &doc, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Cancelled { .. })
        ));
        assert!(app.pending_effect.is_none());
    }
}
