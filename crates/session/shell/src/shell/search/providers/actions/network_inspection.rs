use super::{ActionError, Chrome, Rc, ResourceKind, SearchResult, t};
use crate::shell::search::ActionReceipt;

fn inspect(chrome: &Chrome) -> Result<ActionReceipt, ActionError> {
    let state = chrome.borrow();
    if state.session_locked() || !crate::shell::overlay_host::is_active_workspace_owner(&state) {
        return Err(ActionError::Refused(t(
            "quick-settings-network-inspect-denied",
        )));
    }
    crate::shell::quick_settings::inspection::network(&state)
        .map(ActionReceipt::completed_with)
        .map_err(|key| ActionError::Refused(t(key)))
}

pub(super) fn row() -> SearchResult {
    SearchResult {
        kind: ResourceKind::Action,
        operation_kind: tilo_actions::OperationKind::View,
        provider_id: "actions".into(),
        id: "quick-settings.network.inspect".into(),
        title: t("quick-settings-network-inspect"),
        subtitle: Some(t("quick-settings-network-inspect-subtitle")),
        badge: None,
        score: 0,
        params: vec![],
        contract: Some(tilo_actions::ActionContract::view()),
        on_open: Rc::new(|_, _, chrome, _| inspect(chrome).map(|_| ())),
        on_invoke: Some(Rc::new(|_, _, chrome, _| inspect(chrome))),
        on_begin_invoke: None,
    }
}
