//! The work or school (enterprise, 802.1X) network as Settings sets it up:
//! the setup opens in place under its row, and the join carries what the form
//! holds into the session's `network` resource, its password included.

use std::sync::Arc;

use ui::widget::{DisclosureRow, Widget};
use vello::kurbo::Rect;

use super::{FakeNetwork, PAGE_ADVANCED, model, run, window_showing};
use crate::effects::SettingsEffect;
use crate::i18n::t;
use crate::panes::network::NetworkEffect;

/// The work or school network's setup in the composition: its row, and
/// whether that row is open.
fn enterprise_setup_open(widget: &dyn Widget) -> Option<(bool, Rect)> {
    if let Some(row) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<DisclosureRow>())
        && row
            .accessibility()
            .and_then(|node| node.name)
            .is_some_and(|name| name.starts_with(&t("net-enterprise-entry")))
    {
        return Some((row.is_open(), row.get_rect()));
    }
    widget
        .children()
        .into_iter()
        .find_map(enterprise_setup_open)
}

/// The work or school network's setup opens in place under its row on the
/// standard disclose motion. Its action and a press on the row open the
/// same row: an agent and a person land in the same place.
#[test]
fn the_work_or_school_setup_opens_under_its_row() -> Result<(), String> {
    use ui::headless::viewport;

    let fake = Arc::new(FakeNetwork::default());
    let mut network = model(&fake);
    network.open_page(PAGE_ADVANCED, t("net-advanced"));
    // The networks were just looked for: no periodic scan rebuilds the
    // page between the presses.
    network.scanned_at = Some(ui::time::now());
    let (mut app, network) = window_showing(network, (1000, 1800));
    let shown_open = |app: &mut ui::framework::testing::TestApp<crate::SettingsShell>| {
        app.settle();
        enterprise_setup_open(app.harness().root_widget()).map(|(open, _)| open)
    };
    if shown_open(&mut app) != Some(false) {
        return Err("the setup starts closed, under a disclosure row".into());
    }
    app.effect(&SettingsEffect::from(NetworkEffect::ShowEnterpriseSetup))
        .map_err(|error| error.to_string())?;
    if shown_open(&mut app) != Some(true) {
        return Err("the action opens the row".into());
    }
    app.effect(&SettingsEffect::from(NetworkEffect::ShowEnterpriseSetup))
        .map_err(|error| error.to_string())?;
    app.settle();
    if network.borrow().enterprise_setup.is_open() {
        return Err("the action closes the row again".into());
    }

    let host = app.harness();
    let (open, rect) =
        enterprise_setup_open(host.root_widget()).ok_or("the setup's row is on the page")?;
    if open {
        return Err("the setup is closed before the press".into());
    }
    host.click_at(rect.center().x, rect.y0 + 12.0);
    if !network.borrow().enterprise_setup.is_open() {
        return Err("a press on the row opens the setup the action opens".into());
    }
    host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
        .map_err(|error| error.to_string())?;
    host.click_named("network.enterprise.profile")?;
    host.type_text("Campus");
    if network.borrow().enterprise.profile.get() != "Campus" {
        return Err("the opened setup takes typing".into());
    }
    Ok(())
}

/// The work or school setup form joins through the session's `network`
/// resource: the job carries what the form holds into the action that joins
/// an enterprise network, its password included.
#[test]
fn the_work_or_school_form_joins_through_the_network_resource() {
    let fake = Arc::new(FakeNetwork::default());
    let mut model = model(&fake);
    {
        let form = &model.enterprise;
        form.profile.set("Campus".into());
        form.ssid.set("Campus".into());
        form.identity.set("ada@example.edu".into());
        form.anonymous.set("anonymous@example.edu".into());
        form.ca.set("/etc/ssl/certs/campus.pem".into());
        form.method.set("TTLS".into());
        form.inner_auth.set("PAP".into());
        form.client_certificate.set("/etc/ssl/certs/ada.pem".into());
        form.private_key.set("/etc/ssl/private/ada.key".into());
    }
    run(
        &mut model,
        NetworkEffect::ConnectEnterpriseNetwork {
            profile: None,
            ssid: None,
            identity: None,
            anonymous_identity: None,
            ca_certificate: None,
            method: None,
            inner_auth: None,
            client_certificate: None,
            private_key: None,
            password: Some("hunter2".into()),
        },
    )
    .unwrap();
    let (action, arguments) = fake
        .enterprise_call
        .lock()
        .unwrap()
        .clone()
        .expect("the join reaches the network resource");
    assert_eq!(action, "connect-enterprise-wifi");
    assert_eq!(
        serde_json::Value::Object(arguments),
        serde_json::json!({
            "profile": "Campus",
            "ssid": "Campus",
            "identity": "ada@example.edu",
            "anonymous_identity": "anonymous@example.edu",
            "ca_certificate": "/etc/ssl/certs/campus.pem",
            "method": "TTLS",
            "inner_auth": "pap",
            "client_certificate": "/etc/ssl/certs/ada.pem",
            "private_key": "/etc/ssl/private/ada.key",
            "password": "hunter2",
        })
    );
}
