//! Switching to the next keyboard input source.
//!
//! The input-source chord and the `input.source.next` action do the same
//! thing through [`crate::shell::chrome_state::ChromeState::switch_to_next_input_source`]:
//! persist the next source of the keyboard last typed on, switch the seat to
//! it and say which one it is.

use crate::shell::chrome_state::ChromeState;

impl ChromeState {
    /// Select the keyboard whose sources the next-source chord and action
    /// cycle: the physical keyboard last typed on, the on-screen keyboard,
    /// or the shared sources.
    pub fn set_keyboard_scope(&mut self, scope: String) {
        self.keyboard_scope = scope;
    }

    /// Type with the next input source of the keyboard last typed on: the
    /// choice is saved, the seat switches at once and the readout names
    /// it. The source's name, or `None` when there is only one source.
    pub fn switch_to_next_input_source(&mut self) -> Option<String> {
        let (index, name) = select_next(&self.keyboard_scope)?;
        self.request_input_source(index);
        self.show_osd(crate::shell::osd::Osd::input_source(&name));
        Some(name)
    }
}

/// Make the next keyboard input source the active one in the session's
/// settings, so it survives a settings reload and the next session, and
/// return the keyboard group to select and the source's name. `None` with a
/// single source.
fn select_next(scope: &str) -> Option<(usize, String)> {
    use crate::compositor::backend::drm::input_config::{KeyboardSettings, SECTION};
    let section = settings_store::load_section(SECTION);
    let mut base = settings_store::hardware::RichInputConfig::from_section(&section);
    let mut config = settings_store::keyboard_sources::effective(&base, scope);
    config.select_next_source()?;
    let mut values = serde_json::Map::new();
    if scope == settings_store::keyboard_sources::ALL {
        values.insert(
            "keyboard_layout".into(),
            serde_json::Value::String(config.keyboard_layout.clone()),
        );
    } else {
        let mut inherited = settings_store::keyboard_sources::KeyboardProfile::from_input(&base);
        if let Some(defaults) = base
            .keyboard_assignments
            .get(settings_store::keyboard_sources::ALL)
        {
            inherited.options.clone_from(&defaults.options);
        }
        let mut assignments = base.keyboard_assignments.clone();
        let profile = assignments.entry(scope.into()).or_insert(inherited);
        profile.active.clone_from(&config.keyboard_layout);
        values.insert(
            "keyboard_assignments".into(),
            serde_json::to_value(&assignments).ok()?,
        );
        base.keyboard_assignments = assignments;
    }
    if scope == settings_store::keyboard_sources::ALL {
        base.keyboard_layout.clone_from(&config.keyboard_layout);
    }
    config = settings_store::keyboard_sources::effective(&base, scope);
    if let Err(error) = settings_store::update_section(SECTION, values) {
        tracing::warn!(%error, "input source choice could not be saved");
        return None;
    }
    // The keyboard switches to an XKB group: the active layout, or the
    // primary layout an input-method engine types on. The input-method
    // arbiter picks the engine up from the saved choice.
    let keyboard = KeyboardSettings::from_config(&config);
    let group = keyboard.active_index;
    let name = config.active_input_method().map_or_else(
        || keyboard.source_label(group),
        |engine| engine.label().to_string(),
    );
    Some((group, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::{Chrome, SearchScope};
    use crate::shell::search::action_api::{ActionCatalog, ActionError, AllVisible, ResourceRef};
    use crate::shell::search::providers::actions::ActionsProvider;
    use crate::shell::search::{InvocationDisposition, SearchRegistry};
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use std::rc::Rc;

    /// Invoke `input.source.next` the way the control socket does: resolve
    /// its exact address in the desktop's catalogue, then run it.
    fn invoke_next(chrome: &Chrome) -> Result<InvocationDisposition, ActionError> {
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(ActionsProvider::new()));
        let mut doc = AutoCommit::new();
        let resource = ResourceRef::new("actions", "input.source.next");
        let catalog = ActionCatalog::build_exact(
            &registry,
            &resource,
            SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        let action = catalog
            .select(&resource)
            .ok_or_else(|| ActionError::Refused("input.source.next is not published".into()))?;
        action.invoke_json(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            chrome,
            &serde_json::Map::new(),
        )
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn next_input_source_action_switches_the_keyboard_last_typed_on() {
        let config = tempfile::tempdir().expect("isolated settings directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        // The local person's own workspace, which may switch its seat's
        // sources; an explicit owner keeps the user's registry out of it.
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId::new(), "Desk");
        workspace.set_owner("shell-fixture");
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces.push(workspace);
        let chrome = ChromeState::new_with_fixture_handle(
            WorkspaceTreeHandle::new(tree),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );

        let mut values = serde_json::Map::new();
        values.insert("keyboard_layout".into(), "us".into());
        values.insert("keyboard_layouts".into(), serde_json::json!(["us"]));
        settings_store::update_section("input", values).expect("seed one source");
        let refused = invoke_next(&chrome).expect_err("one source leaves nothing to switch to");
        assert!(
            matches!(&refused, ActionError::Explained(message) if message.contains("Settings")),
            "the refusal names the next step: {refused:?}"
        );
        assert_eq!(chrome.borrow_mut().take_input_source_request(), None);

        let mut values = serde_json::Map::new();
        values.insert("keyboard_layouts".into(), serde_json::json!(["us", "de"]));
        values.insert(
            "keyboard_assignments".into(),
            serde_json::json!({"usb-desk": {"sources": ["de", "us"], "active": "de"}}),
        );
        settings_store::update_section("input", values).expect("seed two sources");
        invoke_next(&chrome).expect("the shared sources switch");
        assert_eq!(
            chrome.borrow_mut().take_input_source_request(),
            Some(1),
            "the seat switches at once, as the chord does"
        );
        let section = settings_store::load_section("input");
        assert_eq!(
            section
                .get("keyboard_layout")
                .and_then(serde_json::Value::as_str),
            Some("de"),
            "the choice is persisted"
        );
        let caption = chrome
            .borrow()
            .osd
            .as_ref()
            .map(crate::shell::osd::Osd::caption);
        assert!(
            matches!(caption.as_deref(), Some("German" | "de")),
            "the readout names the layout: {caption:?}"
        );

        // A keyboard with its own sources cycles those, not the shared ones.
        chrome.borrow_mut().set_keyboard_scope("usb-desk".into());
        invoke_next(&chrome).expect("the keyboard's own sources switch");
        assert_eq!(chrome.borrow_mut().take_input_source_request(), Some(1));
        let section = settings_store::load_section("input");
        assert_eq!(
            section
                .get("keyboard_assignments")
                .and_then(|assignments| assignments.pointer("/usb-desk/active"))
                .and_then(serde_json::Value::as_str),
            Some("us")
        );
        assert_eq!(
            section
                .get("keyboard_layout")
                .and_then(serde_json::Value::as_str),
            Some("de"),
            "the shared active source stays where it was"
        );
    }
}
