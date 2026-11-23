//! The live host withdraws a component's whole action surface when one of its
//! actions breaks its contract: agents, Ctrl+K and screen readers then lose
//! every Settings action at once, and the only trace is a log line per frame.

use settings::SettingsShell;
use settings::categories::Category;
use settings::effects::{open_pane, pane_row_id};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Effect as _;

#[test]
fn open_pane_is_one_published_action_naming_the_pane_it_opens() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let surface = app.agent_surface();
    let rows = surface
        .as_array()
        .expect("the agent surface is a list")
        .iter()
        .filter(|row| row["resource"] == "settings.root" && row["action"] == "open-pane")
        .collect::<Vec<_>>();
    assert_eq!(
        rows.len(),
        1,
        "Settings publishes one open-pane action on its resource: {surface}"
    );
    let params = rows[0]["params"]
        .as_array()
        .expect("open-pane takes params");
    assert!(
        params.iter().any(|param| param["name"] == "pane"),
        "open-pane names the pane it opens: {params:?}"
    );

    app.call_on(
        "settings.root",
        "open-pane",
        serde_json::json!({ "pane": "external_accounts" }),
    )
    .expect("the published action opens the pane");
    app.settle();
    let category = Category::ExternalAccounts.label();
    assert!(
        app.harness().a11y_nodes().iter().any(|(_, node)| {
            node.label() == Some(category.as_str()) && node.is_selected() == Some(true)
        }),
        "the Accounts pane is shown after the action runs"
    );

    let refused = app.call_on(
        "settings.root",
        "open-pane",
        serde_json::json!({ "pane": "no-such-pane" }),
    );
    assert!(
        format!("{refused:?}").contains("no-such-pane"),
        "an unknown pane is refused by name: {refused:?}"
    );
}

/// Every action Settings offered an agent on its own root, as main's walk of the app
/// recorded them: a person's palette, an agent and a screen reader find them there.
const ROOT_ACTIONS_MAIN_OFFERED: &[&str] = &[
    "accept-service",
    "activate-window",
    "add-account",
    "add-device",
    "add-dictionary-word",
    "add-email-account",
    "add-google-gemini",
    "add-google-vertex",
    "add-model-file",
    "add-provider",
    "add-source",
    "apply",
    "apply-display-changes",
    "apply-preview",
    "ask-hotspot-password",
    "ask-wireguard-private-key",
    "assign-speaker-role",
    "back-up-identity",
    "begin-speaker-setup",
    "cancel-account-form",
    "cancel-hugging-face-sign-in",
    "cancel-mobile-form",
    "cancel-new-password",
    "cancel-removal",
    "cancel-sign-in",
    "change-sim-pin",
    "check-for-updates",
    "checkpoint",
    "choose-ca-certificate",
    "choose-language",
    "choose-paste-app",
    "choose-source-to-add",
    "choose-vertex-project",
    "choose-wallpaper",
    "clean-up-caches",
    "close-source-picker",
    "close-speaker-setup",
    "confirm-source-to-add",
    "connect-enterprise-network",
    "continue-speaker-setup",
    "copy-code",
    "create-plugin-vpn",
    "create-speaker-set",
    "create-user",
    "create-wireguard",
    "decline-service",
    "diff",
    "edit-dictionary-word",
    "field-edit",
    "find-printers",
    "forget-secrets",
    "free-cache",
    "free-synced-files",
    "generate-wireguard-keys",
    "group",
    "history",
    "identify-displays",
    "identify-speaker",
    "import-openvpn",
    "import-vpn-file",
    "import-wireguard",
    "import-wireguard-config",
    "inspect-appearance",
    "inspect-calendar-choices",
    "inspect-display-configuration",
    "inspect-input-sources",
    "inspect-keyboard-key",
    "inspect-keyboard-map",
    "inspect-keyboard-sources",
    "inspect-mobile-state",
    "inspect-network-state",
    "inspect-shared-displays",
    "install-codecs",
    "install-firmware",
    "install-model",
    "install-windows-support",
    "join-hidden-network",
    "keep-display-changes",
    "keep-here",
    "link-service",
    "manage-billing",
    "move-source-down",
    "move-source-up",
    "open",
    "open-backups",
    "open-calibration",
    "open-disks",
    "open-hub-model-page",
    "open-invoice",
    "open-link",
    "open-network-sign-in",
    "open-pane",
    "open-problem",
    "open-source-picker",
    "open-workspace-defaults",
    "open-workspace-privacy",
    "outline",
    "play-identify-sound",
    "play-speaker",
    "preview-layout",
    "publish-browser-identity",
    "read",
    "redo",
    "refresh-bluetooth",
    "refresh-codecs",
    "refresh-disk-unlock",
    "refresh-drives",
    "refresh-firmware",
    "refresh-mobile",
    "refresh-networks",
    "refresh-printers",
    "refresh-providers",
    "remove-codecs",
    "remove-dictionary-word",
    "remove-hub-token",
    "remove-source",
    "remove-windows-support",
    "render",
    "reorder-source",
    "replace-source",
    "reset-dictionary",
    "restart-desktop",
    "restart-to-update",
    "restore",
    "restore-reviewed",
    "revert-display-changes",
    "review-model-download",
    "review-restore",
    "review-rollback",
    "revoke-device-answer",
    "roll-back",
    "run-diagnostics",
    "save-account-form",
    "save-hub-token",
    "save-new-password",
    "save-speaker-set",
    "search-hub-models",
    "select-calendar",
    "select-configured-source",
    "select-input",
    "select-keyboard-scope",
    "select-output",
    "select-speaker-set",
    "set-active-source",
    "set-agent-model",
    "set-airplane-mode",
    "set-automatic-time",
    "set-automatic-updates",
    "set-bluetooth",
    "set-brightness",
    "set-cache-size",
    "set-caps-lock",
    "set-charge-limit",
    "set-compose-key",
    "set-data-roaming",
    "set-default-app",
    "set-device-exit",
    "set-device-service",
    "set-device-vpn-kill-switch",
    "set-discovering",
    "set-hotspot",
    "set-hub-format",
    "set-input-volume",
    "set-interaction-model",
    "set-ipv4",
    "set-keyboard-model",
    "set-metered-updates",
    "set-mobile-apn",
    "set-mobile-broadband",
    "set-mobile-metered",
    "set-output-muted",
    "set-output-volume",
    "set-power-profile",
    "set-preferred-network",
    "set-prefetch",
    "set-preview-extra-levels",
    "set-preview-level",
    "set-secrets-access",
    "set-sim-pin-lock",
    "set-source-inspection-visible",
    "set-source-options-visible",
    "set-tablet-output",
    "set-time-zone",
    "set-up-accelerator",
    "set-update-schedule",
    "set-wifi",
    "set-wired",
    "set-work",
    "set-work-room",
    "set-work-while-charging",
    "share-link",
    "show-add-plugin-vpn",
    "show-add-vpn",
    "show-add-wireguard",
    "show-advanced-network-settings",
    "show-all-networks",
    "show-apn-editor",
    "show-connect",
    "show-data-usage",
    "show-enterprise-setup",
    "show-hidden-network",
    "show-hotspot-code",
    "show-hotspot-setup",
    "show-hub-model",
    "show-known-networks",
    "show-live-keyboard-map",
    "show-local-model",
    "show-mobile",
    "show-pair-device",
    "show-setting",
    "show-vpns",
    "show-wired",
    "sign-in-to-provider",
    "sign-in-with-google",
    "sign-in-with-hugging-face",
    "sign-in-with-hugging-face-code",
    "skip-speaker",
    "speaker-setup-back",
    "start-calibration",
    "start-hotspot",
    "storage-summary",
    "test-speaker",
    "toggle-microphone-test",
    "unblock-sim",
    "undo",
    "unlink-service",
    "unlock-sim",
];

/// Every action main's walk of the app found on its own root is still offered: a person's
/// palette, an agent, a script and a screen reader read them from here, and one the app stops
/// publishing is every one of those losing it at once.
#[test]
fn the_root_offers_every_action_main_offered() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let surface = app.agent_surface();
    let offered: std::collections::HashSet<(&str, &str)> = surface
        .as_array()
        .expect("the agent surface is a list")
        .iter()
        .filter_map(|row| Some((row["resource"].as_str()?, row["action"].as_str()?)))
        .collect();
    let missing: Vec<&str> = ROOT_ACTIONS_MAIN_OFFERED
        .iter()
        .copied()
        .filter(|action| !offered.contains(&("settings.root", *action)))
        .collect();
    assert!(
        missing.is_empty(),
        "settings.root no longer offers {missing:?}: {surface}"
    );
}

/// A session publishes a Rust app's resources under a transport alias,
/// `tilo-ui-bound:v1:<host>:<instance>:<hex of the app's own id>`, while the
/// app, its directory and the follow-ups its effects submit name them by that
/// own id. Settings' root stays offered at `settings.root`, and its actions —
/// `choose-language` among them, a job whose follow-up names the root — are
/// found there and run.
#[test]
fn the_root_stays_offered_at_the_apps_own_id_though_the_session_aliases_it() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let surface = app.agent_surface();
    let choose = surface
        .as_array()
        .expect("the agent surface is a list")
        .iter()
        .find(|row| row["resource"] == "settings.root" && row["action"] == "choose-language")
        .unwrap_or_else(|| panic!("Settings offers choose-language on its root: {surface}"));
    assert!(
        choose["params"]
            .as_array()
            .expect("choose-language takes params")
            .iter()
            .any(|param| param["name"] == "language"),
        "choose-language names the language it takes: {choose}"
    );
    // The alias a session published the same root under decodes back to the
    // app's own id, so a follow-up naming `settings.root` names the resource
    // it published.
    let alias = format!(
        "{}session:window:{}",
        ui::tilo_actions::BOUND_RESOURCE_PREFIX,
        hex::encode("settings.root")
    );
    assert_eq!(
        &*ui::tilo_actions::logical_resource_id(&alias),
        "settings.root"
    );
    // The job runs when the app's own address is invoked, as the broker
    // delivers it after resolving that alias.
    app.call_on(
        "settings.root",
        "choose-language",
        serde_json::json!({ "language": "device" }),
    )
    .expect("the root's job effect runs at the app's own id");
    app.settle();
}

#[test]
fn every_pane_publishes_a_surface_that_passes_contract_validation() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let mut failures = Vec::new();
    let violations = app.harness().component_contract_violations();
    if !violations.is_empty() {
        failures.push(format!("on open: {violations:#?}"));
    }
    for category in Category::ALL.iter().copied() {
        app.effect_on(&pane_row_id(category), &open_pane(category))
            .unwrap_or_else(|error| panic!("{} opens: {error:?}", category.slug()));
        app.settle();
        let violations = app.harness().component_contract_violations();
        if !violations.is_empty() {
            failures.push(format!("{}: {violations:#?}", category.slug()));
        }
        let stale = stale_control_bindings(&mut app);
        if !stale.is_empty() {
            failures.push(format!(
                "{}: controls bound to unpublished actions {stale:?}",
                category.slug()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn stale_control_bindings(app: &mut TestApp<SettingsShell>) -> Vec<(String, String)> {
    let resources = app.harness().resources();
    ui::widget::controls::control_action_bindings(app.harness().root_widget())
        .into_iter()
        .filter_map(|binding| match binding.kind {
            ui::widget::ControlBindingKind::Action(address)
                if !address.resource.starts_with("tilo.host:")
                    && ui::tilo_actions::api::resolve(&resources, &address).is_none() =>
            {
                Some((
                    binding.control_id,
                    format!("{}#{}", address.resource, address.action),
                ))
            }
            _ => None,
        })
        .collect()
}

fn navigation_semantics(node: &ui::headless::InspectionNode) -> Option<&ui::a11y::AccessNode> {
    if node.test_id.as_deref() == Some("settings.nav") {
        return node.semantics.as_ref();
    }
    node.children.iter().find_map(navigation_semantics)
}

fn admitted_category_rows(node: &ui::a11y::AccessNode, rows: &mut Vec<(String, String)>) {
    if node.state.hidden {
        return;
    }
    if node.role == ui::a11y::Role::TreeItem
        && Category::ALL
            .iter()
            .any(|category| node.name.as_deref() == Some(category.label().as_str()))
    {
        rows.push((
            node.name.clone().expect("a category has its actual label"),
            node.action_target
                .clone()
                .expect("an admitted row has its actual target"),
        ));
    }
    for child in &node.children {
        admitted_category_rows(child, rows);
    }
}

#[test]
fn a_sidebar_click_opens_the_pane_through_its_effect() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let bindings = app
        .with_widget::<ui::widget::NavShell, _>(|shell| {
            ui::widget::controls::control_action_bindings(shell)
        })
        .expect("settings shows its shell");
    let open = open_pane(Category::Bluetooth);
    let navigation = bindings
        .iter()
        .filter(|binding| {
            matches!(
                &binding.kind,
                ui::widget::ControlBindingKind::Action(address)
                    if address.resource == "settings.root" && address.action == open.id()
            )
        })
        .collect::<Vec<_>>();
    let frame = app.harness().inspection();
    let tree = navigation_semantics(&frame.root).expect("the retained navigation list");
    let mut visible = Vec::new();
    admitted_category_rows(tree, &mut visible);
    assert!(!visible.is_empty(), "the sidebar admits category rows");
    // Each admitted row's control is bound once, and no row the sidebar keeps out is bound.
    // A page's own way to another page is a button of that page, not a category row.
    assert_eq!(
        navigation
            .iter()
            .filter(|binding| binding.control_id.starts_with("key:"))
            .count(),
        visible.len(),
        "every admitted category row has one typed open-pane binding: {bindings:#?}"
    );
    for (label, target) in &visible {
        assert_eq!(
            navigation
                .iter()
                .filter(|binding| &binding.control_id == target)
                .count(),
            1,
            "{label}'s actual row {target} shares the canonical open-pane api"
        );
    }
    let bluetooth = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.role() == ui::accesskit::Role::TreeItem
                && node.label() == Some(Category::Bluetooth.label().as_str()))
            .then(|| node.bounds())
            .flatten()
        })
        .expect("the retained Bluetooth row exposes its actual pointer bounds");
    app.harness().click_at(
        f64::midpoint(bluetooth.x0, bluetooth.x1),
        f64::midpoint(bluetooth.y0, bluetooth.y1),
    );
    app.settle();
    assert_eq!(crate::support::selected(&mut app), Category::Bluetooth);
}

#[test]
fn a_model_link_opens_agent_models() {
    let mut app = TestApp::<SettingsShell>::launched(&["settings://models/u2net-subject"])
        .expect("settings opens on a link");
    let settled = app
        .harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness.refusal_said().is_some()
                || harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Privacy"))
        });
    assert!(
        settled,
        "the link either opens Agent Models or says why not"
    );
    if let Some(refusal) = app.harness().refusal_said().map(str::to_owned) {
        assert!(
            refusal.contains("u2net-subject"),
            "a missing model is named: {refusal}"
        );
        app.harness().key("Escape");
    }
    let shown = app
        .harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness
                .inspection_json()
                .is_ok_and(|frame| frame.contains("Privacy"))
        });
    assert!(shown, "the link opens Agent Models");
    let category = Category::AgentModels.label();
    assert!(
        app.harness().a11y_nodes().iter().any(|(_, node)| {
            node.label() == Some(category.as_str()) && node.is_selected() == Some(true)
        }),
        "the model link selects the canonical Agent Models category"
    );
}

#[test]
fn every_settings_section_is_set_by_its_setters() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let surface = app.agent_surface();
    let published: std::collections::BTreeSet<String> = surface
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["resource"].as_str().map(str::to_owned))
        .collect();
    for kept in [
        "settings.appearance",
        "settings.accessibility",
        "settings.input",
        "settings.power",
        "settings.updates",
        "settings.notifications",
        "settings.audio",
        "settings.display",
        "settings.locale",
        "settings.continuity",
        "settings.desktop-controls",
        "settings.agent-providers",
        "settings.default-apps",
        "settings.clock",
        "settings.display-placement",
        "settings.automatic-display",
        "settings.osk-sources",
    ] {
        assert!(
            !published.contains(&settings_sections::address(kept).unwrap()),
            "the session publishes {kept}, not Settings: {published:?}"
        );
    }

    app.call_on(
        &settings_sections::address("settings.desktop-controls").unwrap(),
        "set-vim-mode",
        serde_json::json!({ "value": true }),
    )
    .unwrap();
    assert!(
        app.settings::<settings::prefs::DesktopControls>()
            .get()
            .vim_mode
    );
    app.call_on(
        &settings_sections::address("settings.appearance").unwrap(),
        "set-high-contrast",
        serde_json::json!({ "value": true }),
    )
    .unwrap();
    assert!(
        app.settings::<settings::prefs::AppearancePrefs>()
            .get()
            .high_contrast
    );
    app.call_on(
        &settings_sections::address("settings.notifications").unwrap(),
        "set-do-not-disturb",
        serde_json::json!({ "value": true }),
    )
    .unwrap();
    assert!(
        app.settings::<settings::prefs::NotificationPrefs>()
            .get()
            .do_not_disturb
    );
    assert!(
        app.call_on(
            &settings_sections::address("settings.appearance").unwrap(),
            "set-ui-scale",
            serde_json::json!({ "value": 9.0 }),
        )
        .is_err(),
        "the section's check refuses a scale out of range"
    );
    assert!(
        (app.settings::<settings::prefs::AppearancePrefs>()
            .get()
            .ui_scale
            - 1.0)
            .abs()
            < f64::EPSILON
    );
}

/// The read Settings publishes answers the device's settings document, so an
/// agent, a script or a walk sees the values the settings' actions keep, whole
/// and section by section, and again when the window opens a second time.
#[test]
fn the_settings_read_answers_the_values_its_actions_keep() {
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().expect("a profile of the test's own");
    let mut app = TestApp::<SettingsShell>::open_data_dir(directory.path())
        .expect("settings opens on its profile");
    let document = app
        .settings::<settings::prefs::PowerPrefs>()
        .document()
        .clone();
    let ui::storage::SettingsDocument::Profile(root) = document else {
        panic!("a window on its own profile keeps its settings there");
    };
    // The document the panes edit, as the process the app runs in reads it: a
    // launched app is installed this same source by its host.
    let source = settings_store::scoped_source(Some(settings_store::profile(&root)));
    let power = settings_sections::address("settings.power").expect("power is a section");

    app.call_on(
        &power,
        "set-suspend-plugged-in",
        serde_json::json!({ "minutes": 30 }),
    )
    .expect("the power section takes the timer");
    app.call_on(
        &power,
        "set-suspend-on-battery",
        serde_json::json!({ "minutes": 45 }),
    )
    .expect("the power section takes the timer");
    app.settle();

    let whole = app
        .call("read", serde_json::json!({}))
        .expect("the read answers")
        .expect("the document is there");
    assert_eq!(whole["power"]["suspend_minutes"], 30);
    assert_eq!(whole["power"]["battery_suspend_minutes"], 45);
    for item in ["settings.power", "power"] {
        let section = app
            .call("read", serde_json::json!({ "item": item }))
            .unwrap_or_else(|error| panic!("the read takes {item:?}: {error:?}"))
            .unwrap_or_else(|| panic!("{item:?} is a section of the document"));
        assert_eq!(section["suspend_minutes"], 30, "read by {item:?}");
        assert_eq!(section["battery_suspend_minutes"], 45, "read by {item:?}");
    }

    // The window opens again on the profile it kept: the choices are there,
    // read back through the same published action.
    drop(app);
    drop(source);
    let mut app = TestApp::<SettingsShell>::open_data_dir(directory.path())
        .expect("settings opens again on its profile");
    let _source = settings_store::scoped_source(Some(settings_store::profile(&root)));
    let kept = app
        .call("read", serde_json::json!({ "item": "power" }))
        .expect("the read answers")
        .expect("the section is there");
    assert_eq!(kept["suspend_minutes"], 30);
    assert_eq!(kept["battery_suspend_minutes"], 45);
}
