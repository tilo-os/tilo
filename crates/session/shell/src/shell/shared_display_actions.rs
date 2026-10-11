//! Shared-display arrangement actions. Observed outputs never grant input authority.

use settings_store::person_displays::{Arrangement, Layout, PersonDisplays};

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};

pub(crate) const INCLUDE_ID: &str = "display.shared.include";
pub(crate) const INSPECT_ID: &str = "display.shared.inspect";
pub(crate) const INCLUDE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "device",
        ParamType::Text,
        "Certified device ID returned by display.shared.inspect",
    ),
    ActionParam::required(
        "output",
        ParamType::Text,
        "Certified output ID returned by display.shared.inspect",
    ),
    ActionParam::required(
        "revision",
        ParamType::Integer,
        "Current arrangement revision of this output, or zero when unarranged",
    ),
    ActionParam::required(
        "included",
        ParamType::Boolean,
        "Whether this output belongs to the shared layout; grants no remote input",
    ),
];

#[derive(Clone)]
pub(crate) struct SharedDisplayView {
    pub facts: PersonDisplays,
    pub arrangement: Arrangement,
    pub layout: Layout,
}

pub(crate) fn current() -> Option<SharedDisplayView> {
    let facts = PersonDisplays::load()?;
    crate::shell::personal_continuity::DeviceId::new(facts.local_device.clone()).ok()?;
    let arrangement = Arrangement::load();
    let layout = Layout::resolve(&facts.devices, &arrangement);
    Some(SharedDisplayView {
        facts,
        arrangement,
        layout,
    })
}

pub(crate) fn inspect() -> Result<serde_json::Value, ActionError> {
    let view = current().ok_or_else(|| ActionError::Refused(t("display-remote-unavailable")))?;
    let outputs = view
        .layout
        .listed()
        .map(|display| {
            let revision = view
                .arrangement
                .entry(&display.device, &display.output)
                .map_or(0, |entry| entry.rev);
            serde_json::json!({
                "device": display.device, "output": display.output, "name": display.name,
                "device_label": display.device_label, "number": display.order.saturating_add(1),
                "local": display.local, "online": display.online, "included": display.enabled,
                "revision": revision, "action": INCLUDE_ID,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({"local_device":view.facts.local_device,"outputs": outputs}))
}

pub(crate) fn include(
    state: &ChromeState,
    device: &str,
    output: &str,
    revision: u64,
    included: bool,
) -> Result<(), ActionError> {
    if !crate::shell::search::providers::actions::current_shell_action_authorized(state, INCLUDE_ID)
    {
        return Err(ActionError::Refused(t("display-remote-permission")));
    }
    let view = current().ok_or_else(|| ActionError::Refused(t("display-remote-unavailable")))?;
    if !view
        .layout
        .listed()
        .any(|display| display.is(device, output))
    {
        return Err(ActionError::Refused(t("display-remote-unavailable")));
    }
    let current_revision = view
        .arrangement
        .entry(device, output)
        .map_or(0, |entry| entry.rev);
    if current_revision != revision {
        return Err(ActionError::Refused(t("display-remote-stale")));
    }
    view.layout
        .set_enabled(&view.arrangement, device, output, included)
        .save()
        .map_err(|error| ActionError::Refused(error.to_string()))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::shell::state::FreshId as _;

    pub(crate) fn publish() -> tilo_test_env::TestHome {
        use settings_store::person_displays::{DeviceOutput, PersonDevice};
        let home = tilo_test_env::TestHome::new().unwrap();
        let (publisher_pid, publisher_start_ticks) =
            settings_store::hardware::publisher_identity().unwrap();
        let devices = [
            ("aa01", "Laptop", true, true),
            ("bb02", "Orange Pi", false, true),
            ("cc03", "Living room TV", false, false),
        ]
        .into_iter()
        .map(|(id, label, local, online)| PersonDevice {
            device: id.into(),
            label: label.into(),
            local,
            online,
            outputs: vec![DeviceOutput {
                id: "panel".into(),
                name: "Desk panel".into(),
                builtin: local,
                x: 0,
                y: 0,
                width: 960,
                height: 540,
                mode_width: 1920,
                mode_height: 1080,
                refresh_mhz: 60_000,
                scale_milli: 2000,
                connected: true,
                enabled: true,
            }],
        })
        .collect();
        PersonDisplays {
            schema_version: settings_store::person_displays::SCHEMA_VERSION,
            publisher_pid,
            publisher_start_ticks,
            local_device: "aa01".into(),
            devices,
        }
        .save()
        .unwrap();
        assert!(
            current().is_some(),
            "the actual live-publisher loader must observe the fixture"
        );
        home
    }

    pub(crate) fn owner() -> crate::shell::chrome_state::Chrome {
        use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, "Display fixture");
        workspace.set_owner("shell-fixture");
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = id;
        crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            WorkspaceTreeHandle::new(tree),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        )
    }
}
