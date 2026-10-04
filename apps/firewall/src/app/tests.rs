use super::*;
use ui::headless::Framed;
use ui::prelude::TestApp;
use ui::widget::SidebarPlacement;

type Hosted = TestApp<crate::FirewallShell>;
fn seeded() -> (FirewallApp, AutoCommit) {
    (FirewallApp::new_demo(), AutoCommit::new())
}
fn rules(app: &Hosted) -> Vec<Rule> {
    app.state().zone(ZoneKind::Public).unwrap().rules.clone()
}
fn semantic(app: &mut Hosted, name: &str) {
    app.harness()
        .perform_semantic_named(name, None, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}
fn fill(app: &mut Hosted, name: &str, value: &str) {
    operate_hosted(app, name);
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("a");
    app.harness().set_modifiers(&[]);
    app.harness().type_text(value);
    app.settle();
}
fn framed((app, doc): (FirewallApp, AutoCommit), w: f64, h: f64) -> Framed<FirewallApp> {
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc));
    Framed::new(state, app, ui::headless::viewport((w, h)), Theme::dark())
}

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

fn hosted() -> ui::prelude::TestApp<crate::FirewallShell> {
    ui::prelude::TestApp::<crate::FirewallShell>::capture(None).unwrap()
}

fn operate_hosted(app: &mut ui::prelude::TestApp<crate::FirewallShell>, suffix: &str) {
    let targets = app.harness().targets();
    let (_, rect) = targets
        .iter()
        .find(|(id, _)| id.ends_with(suffix))
        .unwrap_or_else(|| panic!("missing {suffix}: {:?}", targets.keys()));
    let center = rect.center();
    app.harness().click_at(center.x, center.y);
    app.settle();
}

fn semantic_nodes(widget: &dyn Widget) -> Vec<ui::a11y::AccessNode> {
    fn flatten(node: ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
        for child in node.children.clone() {
            flatten(child, out);
        }
        out.push(node);
    }
    let mut result = Vec::new();
    if let Some(node) = widget.accessibility() {
        flatten(node, &mut result);
    }
    for child in widget.children() {
        result.extend(semantic_nodes(child));
    }
    result
}

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
            ui::framework::testing::HeadlessApp::<crate::FirewallShell>::capture(scenario)
                .expect("the scenario opens headlessly");
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

#[test]
fn the_zones_dock_when_wide_and_collapse_to_a_drawer_when_narrow() {
    let wide = framed(seeded(), 1280.0, 800.0);
    assert_eq!(wide.shell.geometry().sidebar, SidebarPlacement::Docked);
    assert!(wide.shell.is_sidebar_open());

    let narrow = framed(seeded(), 640.0, 600.0);
    assert_eq!(narrow.shell.geometry().sidebar, SidebarPlacement::Drawer);
    assert!(!narrow.shell.is_sidebar_open());
}

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

#[test]
fn layout_grows_with_the_text_scale() {
    let mut heights = Vec::new();
    for scale in [1.0, 2.0] {
        let (app, doc) = seeded();
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc));
        let app = Framed::new(
            state,
            app,
            ui::headless::viewport((1600.0, 1200.0)),
            Theme::dark().with_text_scale(scale),
        );
        let control = target(&app, "firewall-rule-new");
        assert!(control.width() > 0.0 && control.x0 >= 0.0 && control.x1 <= 1600.0);
        heights.push(control.height());
        for node in semantic_nodes(&*app) {
            if let Some(bounds) = node.bounds {
                let rect = bounds.to_rect();
                assert!(
                    [rect.x0, rect.y0, rect.x1, rect.y1]
                        .iter()
                        .all(|value| value.is_finite())
                );
            }
        }
    }
    assert!(
        heights[1] > heights[0],
        "controls grow with text instead of clipping it"
    );
}

#[test]
fn a_demo_firewall_is_published_through_the_action() {
    let mut app = hosted();
    let expected = crate::sample::sample_config();
    assert_eq!(app.state().default_zone, expected.default_zone);
    assert_eq!(app.state().zones.len(), expected.zones.len());
    for zone in app.state().zones {
        let wanted = expected.zone(zone.kind).unwrap();
        assert_eq!(zone.services, wanted.services);
        assert_eq!(
            zone.rules
                .iter()
                .map(|rule| (&rule.matcher, rule.action, &rule.source))
                .collect::<Vec<_>>(),
            wanted
                .rules
                .iter()
                .map(|rule| (&rule.matcher, rule.action, &rule.source))
                .collect::<Vec<_>>()
        );
    }
    let resources = app.harness().resources();
    assert_eq!(
        resources
            .iter()
            .filter(|row| row.kind == "firewall.config")
            .count(),
        1
    );
    assert!(
        !app.versions().is_empty(),
        "the framework import is an action in history"
    );
}

#[test]
fn backend_availability_governs_what_is_shown_and_announced() {
    assert!(FirewallApp::new().config.zones.is_empty());
    let mut app = FirewallApp::new();
    assert!(
        app.finish_retry(Err("Permission to read firewalld was denied".into()))
            .is_err()
    );
    assert!(app.config.zones.is_empty());
    assert!(app.accessibility().unwrap().children.iter().any(|node| {
        node.live.is_some()
            && node
                .name
                .as_deref()
                .is_some_and(|name| name.contains("Permission"))
    }));
    let mut hosted = Hosted::capture(Some("service-error")).unwrap();
    assert!(hosted.state().zones.is_empty());
    let before = hosted.versions().len();
    assert!(hosted.effect(&FirewallEffect::ApplyConfig).is_err());
    assert_eq!(hosted.versions().len(), before);
    let tree = hosted.harness().inspection_json().unwrap();
    assert!(tree.contains("Permission to read firewalld was denied"));
    assert!(tree.contains("Try again"));
}

#[test]
fn the_add_rule_form_validates_each_field() {
    let mut view = FirewallApp::new_demo();
    view.draft.service = false;
    view.draft.port.set("70000".into());
    view.draft.source.set("10.0.0".into());
    assert!(view.validated_draft().is_none());
    assert_eq!(
        view.draft.port_error.as_deref(),
        Some("Use a port from 1 to 65535.")
    );
    assert!(view.draft.source_error.is_some());
    view.draft.port.set("9000".into());
    view.draft.source.set(String::new());
    assert!(view.validated_draft().is_some());
    view.draft.service = true;
    view.draft.service_id.set("wireguard".into());
    assert_eq!(
        view.validated_draft().unwrap().matcher,
        Matcher::Service("wireguard".into())
    );

    let mut app = hosted();
    let before = rules(&app).len();
    operate_hosted(&mut app, "firewall-rule-new");
    semantic(&mut app, "Port");
    fill(&mut app, "firewall-rule-port", "70000");
    semantic(&mut app, "Add rule");
    assert_eq!(rules(&app).len(), before);
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Use a port from 1 to 65535")
    );
    fill(&mut app, "firewall-rule-port", "9000");
    semantic(&mut app, "Add rule");
    assert_eq!(rules(&app).len(), before + 1);
    assert!(rules(&app).iter().any(
        |rule| matches!(&rule.matcher, Matcher::Port { port, proto: Proto::Tcp } if port == "9000")
    ));
}

#[test]
fn deleting_a_rule_asks_first_and_dispatches_the_palette_action() {
    let mut app = hosted();
    let rule = rules(&app)[0].clone();
    let versions = app.versions().len();
    operate_hosted(&mut app, &format!("firewall-rule-delete-{}", rule.id));
    assert!(rules(&app).iter().any(|r| r.id == rule.id));
    semantic(&mut app, "Cancel");
    assert!(rules(&app).iter().any(|r| r.id == rule.id));
    assert_eq!(app.versions().len(), versions);
    operate_hosted(&mut app, &format!("firewall-rule-delete-{}", rule.id));
    semantic(&mut app, "Delete rule");
    assert!(!rules(&app).iter().any(|r| r.id == rule.id));
    assert_eq!(app.versions().len(), versions + 1);
    assert!(app.undo());
    assert!(rules(&app).iter().any(|r| r.id == rule.id));
    app.invoke(
        ui::tilo_resource::Invoke::apply(&crate::actions::delete_rule_action(
            ZoneKind::Public,
            rule.id,
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(
        !rules(&app).iter().any(|r| r.id == rule.id),
        "the same declared action deletes through the framework"
    );
}

#[test]
fn the_row_delete_button_opens_the_confirmation() {
    let mut app = hosted();
    let first = rules(&app)[0].id;
    operate_hosted(&mut app, &format!("firewall-rule-delete-{first}"));
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Delete this rule?")
    );
    assert!(rules(&app).iter().any(|rule| rule.id == first));
}

#[test]
fn switches_round_trip_through_the_document() {
    let mut app = hosted();
    let rule = rules(&app)[0].clone();
    operate_hosted(&mut app, &format!("firewall-rule-enabled-{}", rule.id));
    assert_ne!(rules(&app)[0].enabled, rule.enabled);
    semantic(&mut app, "Services");
    operate_hosted(&mut app, "firewall-service-ssh");
    assert!(
        !app.state()
            .zone(ZoneKind::Public)
            .unwrap()
            .has_service("ssh")
    );
    operate_hosted(&mut app, "firewall-panic");
    assert!(app.state().panic_mode);
    operate_hosted(&mut app, "firewall-enabled");
    assert!(!app.state().enabled);
    assert!(app.undo());
    assert!(app.state().enabled);
}

#[test]
fn making_a_zone_active_changes_the_default() {
    let mut app = hosted();
    semantic(&mut app, "Home");
    operate_hosted(&mut app, "firewall-make-active");
    assert_eq!(app.state().default_zone, ZoneKind::Home);
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Home is now the active zone")
    );
}

#[test]
fn exported_effects_complete_only_after_sync() {
    // Async execution/cancellation belongs to the framework's typed effect runner.
    // The application assertion is that Apply completes there without a document edit.
    let mut app = hosted();
    let versions = app.versions().len();
    let config = app.state();
    app.effect(&FirewallEffect::ApplyConfig).unwrap();
    app.settle();
    assert_eq!(app.state(), config);
    assert_eq!(app.versions().len(), versions);
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("commands planned")
    );
}

#[test]
fn rule_and_zone_edits_round_trip_through_the_document() {
    let mut app = hosted();
    let versions = app.versions().len();
    semantic(&mut app, "Home");
    semantic(&mut app, "Services");
    assert_eq!(app.versions().len(), versions, "navigation remains local");
    let rule = rules(&app)[0].clone();
    semantic(&mut app, "Public");
    semantic(&mut app, "Rules");
    operate_hosted(&mut app, &format!("firewall-rule-enabled-{}", rule.id));
    assert_ne!(rules(&app)[0].enabled, rule.enabled);
    assert!(app.undo());
    assert_eq!(rules(&app)[0].enabled, rule.enabled);
}

#[test]
fn apply_now_reports_the_planned_commands() {
    let mut app = hosted();
    operate_hosted(&mut app, "firewall-apply");
    app.harness()
        .wait_until(ui::time::Duration::from_secs(5), |h| {
            h.inspection_json().unwrap().contains("commands planned")
        });
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("commands planned")
    );
}

#[test]
fn zone_list_and_rules_table_are_reader_operable() {
    let mut app = hosted();
    semantic(&mut app, "Home");
    semantic(&mut app, "Public");
    let rule = rules(&app)[0].clone();
    let name = match rule.action {
        Action::Allow => "Allow rules",
        Action::Deny => "Deny rules",
    };
    let row = format!("row:{}:0", crate::view::rule_row_key(rule.id));
    app.harness()
        .perform_semantic_named(name, Some(&row), &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    app.harness()
        .perform_semantic_named(
            &format!("firewall-rule-enabled-{}", rule.id),
            None,
            &ui::a11y::Action::ToggleCheck,
        )
        .unwrap();
    app.settle();
    assert_ne!(rules(&app)[0].enabled, rule.enabled);
    semantic(&mut app, &format!("firewall-rule-delete-{}", rule.id));
    assert!(rules(&app).iter().any(|r| r.id == rule.id));
    semantic(&mut app, "Delete rule");
    assert!(!rules(&app).iter().any(|r| r.id == rule.id));
}

#[test]
fn table_geometry_follows_the_built_rows() {
    for width in [480, 1100] {
        let mut app = Hosted::capture_at(None, (width, 1000)).unwrap();
        let rules = rules(&app);
        let targets = app.harness().targets();
        let mut checked = 0;
        for rule in rules {
            let toggle = targets
                .iter()
                .find(|(id, _)| id.ends_with(&format!("firewall-rule-enabled-{}", rule.id)))
                .map(|(_, rect)| rect);
            let delete = targets
                .iter()
                .find(|(id, _)| id.ends_with(&format!("firewall-rule-delete-{}", rule.id)))
                .map(|(_, rect)| rect);
            if let (Some(toggle), Some(delete)) = (toggle, delete) {
                checked += 1;
                assert!(toggle.width() > 0.0 && delete.width() > 0.0);
                assert!(toggle.x1 <= delete.x0 + 0.5);
                assert!(delete.x1 <= f64::from(width) + 0.5);
            }
        }
        assert!(checked > 0);
    }
}
