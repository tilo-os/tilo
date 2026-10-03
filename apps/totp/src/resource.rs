//! Authenticator as a [`tilo_resource::Resource`]: secret-free account metadata
//! in the document; codes and vault writes are View/Effect operations.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

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
                action: None,
            })
            .collect()
    }
}

/// Framework app: the authenticator widget over [`Totp`].
pub struct TotpShell;

impl ResourceApp for TotpShell {
    type Resource = Totp;
    const ID: &'static str = "so.tilo.totp";
    const TITLE: &'static str = "Authenticator";
    const ICON: Option<&'static str> = Some("totp");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Totp>) -> Element {
        cx.surface(AuthenticatorApp::new())
    }
}

impl ResourceSurface<Totp> for AuthenticatorApp {
    fn present(&mut self, state: &std::rc::Rc<Vec<Account>>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Totp>) {
        self.attach_framework(cx.dispatcher());
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

/// Live launch with capture protection for one-time codes.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    // Window title / capture protection live in main.rs via configure_app
    // (native App only). The browser mounts the definition without that path.
    ui::framework::launch_spec::<TotpShell>()
}
