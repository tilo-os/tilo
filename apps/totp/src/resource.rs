//! Authenticator as a [`tilo_resource::Resource`]: secret-free account metadata
//! in the document; codes and vault writes are View/Effect operations.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::icons::Icon;
use ui::search::SearchRequest;
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::{Button, ButtonRole, Element, NavShell, history_button};

use crate::actions::{
    TOTP_ACTION_TYPE, TotpAction, account_resource_id, add_account_view, register,
};
use crate::app::AuthenticatorApp;
use crate::doc::AuthDoc;
use crate::model::Account;

/// The authenticator vault resource (metadata only; secrets stay local).
pub struct Totp;

impl Resource for Totp {
    const KIND: &'static str = "totp.vault";
    const LABEL: &'static str = "Accounts";
    const ACTION_TYPE: &'static str = TOTP_ACTION_TYPE;

    type State = Vec<Account>;
    type Action = TotpAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Vec<Account>, ModelError> {
        Ok(AuthDoc::list_accounts(doc))
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        let _ = AuthDoc::accounts_map(doc);
        Ok(())
    }

    fn local_operations() -> Vec<ActionSpec> {
        vec![add_account_view()]
    }

    fn children(state: &Vec<Account>) -> Vec<tilo_actions::Resource> {
        // Nested account rows for the published tree. Document actions on each
        // account still come from the existing builders until those move into
        // per-child `declarations()`; codes require a local secret (Effect).
        let count = state.len();
        state
            .iter()
            .enumerate()
            .map(|(index, account)| {
                crate::actions::provide_account_resource(account, index, count, false)
            })
            .collect()
    }

    fn search(state: &Vec<Account>) -> Vec<SearchEntry> {
        state
            .iter()
            .map(|account| SearchEntry {
                anchor: Some(account_resource_id(account.id)),
                title: account.title().to_owned(),
                text: account.label.clone(),
            })
            .collect()
    }
}

/// The capture scenarios: the list, and each state the app has a screen for.
const SCENARIOS: &[&str] = &[
    "list",
    "empty",
    "add",
    "add-advanced",
    "provision",
    "missing-secret",
    "persistence-error",
];

/// Framework app: the navigation shell, whose header row carries the search
/// and add buttons, around the authenticator widget over [`Totp`].
pub struct TotpShell;

impl ResourceApp for TotpShell {
    type Resource = Totp;
    const ID: &'static str = "so.tilo.totp";
    const TITLE: &'static str = "Authenticator";
    const ICON: Option<&'static str> = Some("totp");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Totp>) -> Element {
        // The header's add button counts its presses; the content opens the
        // add sheet whenever the count moves.
        let add_requests = cx.local(0_u64);
        let presses = add_requests.clone();
        let add = Button::new("totp-add", "Add account", None)
            .with_icon(Icon::Plus)
            .role(ButtonRole::Primary)
            .on_click(move || presses.update(|count| *count += 1));
        let search = cx.search_button(SearchRequest::this_app());
        Box::new(
            NavShell::new()
                .content(cx.surface(AuthenticatorApp::new().with_add_requests(add_requests)))
                .content_header(
                    "Accounts",
                    vec![Box::new(history_button()), Box::new(search), Box::new(add)],
                ),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }
}

impl ResourceSurface<Totp> for AuthenticatorApp {
    fn present(&mut self, state: &std::rc::Rc<Vec<Account>>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Totp>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Totp>>,
    #[cfg(test)]
    test_hosted: bool,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            #[cfg(test)]
            test_hosted: false,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Totp>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        #[cfg(test)]
        if self.test_hosted {
            return true;
        }
        self.framework.is_some()
    }

    /// Mark the sink hosted without a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.test_hosted = true;
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &TotpAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}

/// The launch specification of the framework app: live window, `--shots` and
/// the other standard modes.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<TotpShell>()
}
