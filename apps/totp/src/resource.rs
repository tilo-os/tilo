//! Authenticator as a [`tilo_resource::Resource`]: secret-free account
//! metadata in the document, each account's seed in this device's secret
//! store, and what only this device can do with a seed (show and copy codes,
//! set one up) as effects.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::secrets::SecretStore;
use ui::tilo_actions::{ActionAddress, ResourcePresentation};
use ui::tilo_resource::{self, Doc, Effect, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    TOTP_ACTION_TYPE, TotpAction, account_label, account_resource_id, add_account_action, register,
};
use crate::app::AuthenticatorApp;
use crate::doc::AuthDoc;
use crate::model::{Account, demo_accounts};

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

    fn children(state: &Vec<Account>) -> Vec<tilo_actions::Resource> {
        let count = state.len();
        state
            .iter()
            .enumerate()
            .map(|(index, account)| crate::actions::account_resource(account, index, count))
            .collect()
    }

    fn search(state: &Vec<Account>) -> Vec<SearchEntry> {
        state
            .iter()
            .map(|account| {
                let id = account_resource_id(account.id);
                SearchEntry::new(account.title(), account.label.clone())
                    .invoking(ActionAddress::new(id.clone(), "open"))
                    .anchored(id)
            })
            .collect()
    }
}

/// What the Authenticator does with what only this device holds: the add
/// form, an account's codes and its secret.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "totp.effect")]
pub enum TotpEffect {
    /// Add account.
    ///
    /// Opens the protected form on this device: a setup link carries the
    /// secret, so it is never an action argument.
    #[effect(permission = "read")]
    AddAccount,
    /// Open account.
    #[effect(permission = "read")]
    Open {
        #[param("Id of the account")]
        account: String,
    },
    /// Copy current code.
    #[effect(privileged, permission = "read")]
    CopyCode {
        #[param("Id of the account")]
        account: String,
    },
    /// Set up on this device.
    ///
    /// Opens the protected form for the account's secret on this device.
    #[effect(permission = "read")]
    SetUpLocal {
        #[param("Id of the account")]
        account: String,
    },
    /// Retry saving secrets.
    ///
    /// Writes the secrets this device's store refused, and reads the ones it
    /// could not.
    RetrySave,
}

/// The secrets a screenshot shows, in place of this device's store.
pub struct ShotSecrets(pub SecretStore);

/// The screenshot scenarios.
pub const SCENARIOS: &[&str] = &[
    "list",
    "add",
    "add-advanced",
    "add-picker",
    "empty",
    "provision",
    "persistence-error",
    "missing-secret",
];

/// Framework app: the authenticator widget over [`Totp`].
pub struct TotpShell;

impl ResourceApp for TotpShell {
    type Resource = Totp;
    const ID: &'static str = "so.tilo.totp";
    const TITLE: &'static str = "Authenticator";
    const ICON: Option<&'static str> = Some("totp");
    // The window shows secrets: captures, recordings and remote viewers get
    // a placeholder in every launch.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        capture_protected: true,
        ..ui::application::ComponentBehaviour::DEFAULT
    };

    fn view(cx: &mut ViewCx<Totp>) -> Element {
        cx.surface(AuthenticatorApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    /// Demo seeds in a store of the capture's own: a screenshot never reads
    /// or writes this device's secrets.
    fn fixture(fixture: &mut Fixture<Totp>) {
        let store = SecretStore::memory(Self::ID);
        let without_first = matches!(fixture.scenario(), Some("provision" | "missing-secret"));
        let mut seeds = crate::secrets::Seeds::new(store.clone(), true);
        for (index, account) in demo_accounts().iter().enumerate() {
            if index == 0 && without_first {
                continue;
            }
            seeds.put(account.id, &account.secret);
        }
        fixture.provide(ShotSecrets(store));
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Totp>> {
        if name == Some("empty") {
            return Vec::new();
        }
        demo_accounts()
            .iter()
            .filter_map(|account| Invoke::apply(&add_account_action(account, account.order)).ok())
            .collect()
    }
}

impl ResourceSurface<Totp, TotpEffect> for AuthenticatorApp {
    fn present(&mut self, state: &Rc<Vec<Account>>) {
        self.present_accounts(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Totp>) {
        let store = match cx.fixture::<ShotSecrets>() {
            Some(shot) => shot.0.clone(),
            None => cx.secrets(),
        };
        self.attach_framework(cx.dispatcher(), store, cx.mode().is_live());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: TotpEffect, _cx: &mut EffectCx<Totp, Self>) -> EffectAnswer {
        self.perform_effect(effect).map(|()| Effected::done())
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.persistence_pending() {
            Vec::new()
        } else {
            vec![EffectStatus::unavailable(
                TotpEffect::RETRY_SAVE,
                "nothing_to_save",
                "Every secret on this device is saved",
            )]
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.accounts()
            .iter()
            .map(|account| self.account_row(account))
            .collect()
    }
}

impl AuthenticatorApp {
    /// What this device adds to a published account: opening it, and the
    /// codes and setup only its own secret allows.
    fn account_row(&self, account: &Account) -> LiveRow {
        let id = account.id.to_string();
        let ready = self.has_local_secret(account.id);
        let mut copy = TotpEffect::CopyCode {
            account: id.clone(),
        }
        .bound();
        if !ready {
            copy.contract.availability = tilo_actions::Availability::Unavailable {
                code: "local_secret_required".into(),
                reason: "Set up this account on this device before copying a code".into(),
            };
        }
        let mut row = LiveRow::new(
            account_resource_id(account.id),
            "totp.account",
            account_label(account),
        )
        .operation(
            TotpEffect::Open {
                account: id.clone(),
            }
            .bound(),
        )
        .operation(copy)
        .presentation(ResourcePresentation {
            detail: Some(
                if ready {
                    "Codes available on this device"
                } else {
                    "Local setup required to generate codes"
                }
                .into(),
            ),
            badge: Some(if ready { "Ready" } else { "Set up" }.into()),
            preview: None,
        });
        if !ready {
            row = row.operation(TotpEffect::SetUpLocal { account: id }.bound());
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use ui::prelude::TestApp;
    use ui::secrets::SecretBackend;
    use ui::tilo_actions::{Availability, ConfirmationPolicy, OperationKind, RiskLevel};
    use vello::kurbo;

    use super::*;
    use crate::actions::{account_resource_id, move_down_action};
    use crate::secrets::flaky::Flaky;

    const GITHUB: &str =
        "otpauth://totp/GitHub:ada@example.com?secret=JBSWY3DPEHPK3PXP&issuer=GitHub";
    const SECRET: &str = "JBSWY3DPEHPK3PXP";

    /// Every app's secrets in one store the test can read.
    struct Store(Arc<dyn SecretBackend>);

    impl ui::host::SecretsProvider for Store {
        fn store(&self, app_id: &str) -> SecretStore {
            SecretStore::with_backend(app_id, Arc::clone(&self.0))
        }
    }

    /// The app on a fresh profile whose secrets live in `backend`.
    fn open(backend: Arc<dyn SecretBackend>) -> (TestApp<TotpShell>, ui::host::HostScope) {
        let services = ui::host::HostServices {
            secrets: Arc::new(Store(backend)),
            ..ui::host::HostServices::null()
        };
        let scope = ui::host::enter(&services);
        (TestApp::<TotpShell>::new().unwrap(), scope)
    }

    /// Where the window shows the semantic target `target`.
    fn bounds(node: &ui::headless::InspectionNode, target: &str) -> Option<kurbo::Rect> {
        fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<kurbo::Rect> {
            if node.action_target.as_deref() == Some(target) {
                return node.bounds.map(ui::a11y::AccessBounds::to_rect);
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        node.semantics
            .as_ref()
            .and_then(|semantics| find(semantics, target))
            .or_else(|| node.children.iter().find_map(|child| bounds(child, target)))
    }

    /// Click what the window shows as `target`, as a person does.
    fn click(app: &mut TestApp<TotpShell>, target: &str) {
        let root = app.harness().inspection().root;
        let rect = bounds(&root, target).unwrap_or_else(|| panic!("the window shows no {target}"));
        let centre = rect.center();
        app.harness().click_at(centre.x, centre.y);
        app.settle();
    }

    /// Type `text` into the field `target`.
    fn type_into(app: &mut TestApp<TotpShell>, target: &str, text: &str) {
        click(app, target);
        app.harness().type_text(text);
        app.settle();
    }

    /// Add an account through the protected form, as a person does.
    fn add_through_the_form(app: &mut TestApp<TotpShell>, uri: &str) {
        app.effect(&TotpEffect::AddAccount).unwrap();
        app.settle();
        click(app, "totp.dialog.link");
        type_into(app, "totp.dialog.uri", uri);
        click(app, "totp.dialog.fill");
        click(app, "totp.dialog.save");
    }

    fn published(app: &mut TestApp<TotpShell>, id: &str) -> tilo_actions::Resource {
        fn find(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
            resources.iter().find_map(|resource| {
                if resource.id.as_str() == id {
                    Some(resource.clone())
                } else {
                    find(&resource.children, id)
                }
            })
        }
        find(&app.harness().resources(), id).unwrap_or_else(|| panic!("{id} is published"))
    }

    fn holds(bytes: &[u8], secret: &str) -> bool {
        bytes
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())
    }

    #[test]
    fn every_launch_withholds_the_codes_window_from_captures() {
        const { assert!(<TotpShell as ResourceApp>::BEHAVIOUR.capture_protected) };
    }

    #[test]
    fn an_added_account_keeps_its_secret_on_this_device_and_its_name_in_the_document() {
        let backend = ui::secrets::memory_backend();
        let (mut app, _host) = open(Arc::clone(&backend));
        add_through_the_form(&mut app, GITHUB);

        let accounts = app.state();
        assert_eq!(accounts.len(), 1, "the add is one document action");
        let account = &accounts[0];
        assert_eq!(account.issuer, "GitHub");
        assert_eq!(account.label, "ada@example.com");
        assert!(account.secret.is_empty());
        assert!(
            !app.read_document(|doc| holds(&doc.clone().save(), SECRET)),
            "the shared document and its history never hold the secret"
        );
        let stored = SecretStore::with_backend(TotpShell::ID, backend)
            .item("seed", &account.id.to_string())
            .get_blocking()
            .unwrap()
            .expect("the secret is this device's item secret for the account");
        assert_eq!(stored.expose(), SECRET.as_bytes());

        // The published account carries its document actions and, from this
        // device, its codes.
        let row = published(&mut app, &account_resource_id(account.id));
        assert_eq!(row.kind, "totp.account");
        let ids: Vec<&str> = row
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for want in [
            "rename",
            "relabel",
            "set-color",
            "move-up",
            "move-down",
            "delete",
            "open",
            "copy-code",
        ] {
            assert!(ids.contains(&want), "account offers {want}: {ids:?}");
        }
        assert!(!ids.contains(&"set-up-local"), "it is set up here");
        let badge = row.presentation.as_ref().and_then(|p| p.badge.as_deref());
        assert_eq!(badge, Some("Ready"));
        let copy = row.actions.iter().find(|a| a.id == "copy-code").unwrap();
        assert_eq!(copy.kind, OperationKind::Effect);
        assert_eq!(copy.contract.risk, RiskLevel::Privileged);
        assert_eq!(copy.contract.confirmation, ConfirmationPolicy::Confirm);
        let delete = row.actions.iter().find(|a| a.id == "delete").unwrap();
        assert_eq!(delete.kind, OperationKind::Document);
        assert_eq!(delete.contract.risk, RiskLevel::Destructive);
        assert!(
            row.actions
                .iter()
                .all(|action| !action.label.contains(SECRET)),
            "no published action names the secret"
        );

        // Copying through the door puts the live code on the clipboard.
        let code = crate::totp::current_code(&Account {
            secret: SECRET.into(),
            ..account.clone()
        });
        app.effect_confirmed(&TotpEffect::CopyCode {
            account: account.id.to_string(),
        })
        .unwrap();
        let copied = ui::clipboard_paste().unwrap_or_default();
        assert!(
            copied == code
                || copied
                    == crate::totp::current_code(&Account {
                        secret: SECRET.into(),
                        ..account.clone()
                    }),
            "copied {copied:?}, the code is {code:?}"
        );
    }

    #[test]
    fn a_shared_account_without_a_secret_here_offers_setup_and_attaches_it() {
        let backend = ui::secrets::memory_backend();
        let (mut app, _host) = open(Arc::clone(&backend));
        // A collaborator's account arrives: metadata only.
        let mut remote = demo_accounts().remove(0);
        let secret = std::mem::take(&mut remote.secret);
        app.invoke(Invoke::apply(&add_account_action(&remote, 0)).unwrap())
            .unwrap();
        let id = account_resource_id(remote.id);

        let row = published(&mut app, &id);
        let copy = row.actions.iter().find(|a| a.id == "copy-code").unwrap();
        assert!(matches!(
            copy.contract.availability,
            Availability::Unavailable { ref code, .. } if code == "local_secret_required"
        ));
        assert!(row.actions.iter().any(|a| a.id == "set-up-local"));
        assert_eq!(
            row.presentation.as_ref().and_then(|p| p.badge.as_deref()),
            Some("Set up")
        );
        assert!(
            app.effect_confirmed(&TotpEffect::CopyCode {
                account: remote.id.to_string()
            })
            .is_err(),
            "no code without the secret"
        );

        app.call_on(&id, "set-up-local", serde_json::json!({}))
            .unwrap();
        app.settle();
        type_into(&mut app, "totp.dialog.secret", &secret);
        click(&mut app, "totp.dialog.save");

        let row = published(&mut app, &id);
        assert_eq!(
            row.presentation.as_ref().and_then(|p| p.badge.as_deref()),
            Some("Ready")
        );
        assert!(!row.actions.iter().any(|a| a.id == "set-up-local"));
        assert_eq!(
            app.state()[0].issuer,
            remote.issuer,
            "setup changes no metadata"
        );
        assert!(
            SecretStore::with_backend(TotpShell::ID, backend)
                .item("seed", &remote.id.to_string())
                .present_blocking()
        );
        assert!(!app.read_document(|doc| holds(&doc.clone().save(), &secret)));
    }

    #[test]
    fn account_edits_are_undoable_document_actions_and_delete_asks_first() {
        let backend = ui::secrets::memory_backend();
        let (mut app, _host) = open(Arc::clone(&backend));
        add_through_the_form(&mut app, GITHUB);
        add_through_the_form(
            &mut app,
            "otpauth://totp/AWS:ops?secret=NB2W45DFOIZA&issuer=AWS",
        );
        let [first, second] = [app.state()[0].id, app.state()[1].id];
        let row = account_resource_id(first);

        app.call_on(&row, "move-down", serde_json::json!({}))
            .unwrap();
        assert_eq!(
            app.state()[0].id,
            second,
            "the reorder reached the document"
        );
        assert!(app.undo());
        assert_eq!(app.state()[0].id, first, "and is one undo step");
        app.invoke(Invoke::apply(&move_down_action(first)).unwrap())
            .unwrap();
        assert_eq!(app.state()[0].id, second, "the same edit through `apply`");

        let delete = Invoke::apply(&crate::actions::delete_account_action(first)).unwrap();
        assert!(
            matches!(
                app.invoke(delete.clone()),
                Err(tilo_actions::DoorError::ConfirmationRequired)
            ),
            "deleting an account asks first, however it is reached"
        );
        assert_eq!(app.state().len(), 2);
        app.invoke_confirmed(delete).unwrap();
        app.settle();
        assert_eq!(
            app.state().len(),
            1,
            "the confirmed delete left the document"
        );
        assert!(
            !SecretStore::with_backend(TotpShell::ID, backend)
                .item("seed", &first.to_string())
                .present_blocking(),
            "and its secret left this device"
        );
    }

    #[test]
    fn an_edit_changes_only_the_fields_it_touched() {
        let (mut app, _host) = open(ui::secrets::memory_backend());
        add_through_the_form(&mut app, GITHUB);
        let id = app.state()[0].id;
        app.invoke(Invoke::apply(&crate::actions::set_color_action(id, 5)).unwrap())
            .unwrap();
        let mut peer = app.peer_document();
        crate::doc::AuthDoc::set_label(&mut peer, id, "ada@corp.example");

        click(&mut app, &format!("totp.select:{id}"));
        click(&mut app, "totp.edit");
        click(&mut app, "totp.dialog.advanced");
        click(&mut app, "totp.dialog.digits:8");
        click(&mut app, "totp.dialog.save");
        app.merge_peer(&mut peer).unwrap();
        app.settle();

        let account = &app.state()[0];
        assert_eq!(account.digits, 8, "the edit reached the document");
        assert_eq!(
            account.label, "ada@corp.example",
            "the collaborator's label stands"
        );
        assert_eq!(account.color, 5, "the edit keeps the account's colour");
        assert!(app.undo(), "the edit is one undo step");
        assert_eq!(app.state()[0].digits, 6);
    }

    #[test]
    fn a_refused_secret_is_reported_and_saved_by_a_retry_the_user_asks_for() {
        let backend = Flaky::new();
        let (mut app, _host) = open(backend.clone());
        backend.down.store(true, Ordering::SeqCst);
        add_through_the_form(&mut app, GITHUB);
        assert_eq!(app.state().len(), 1, "the account is shared anyway");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("isn't saved"), "{frame}");
        assert!(
            app.effect(&TotpEffect::RetrySave).is_err(),
            "the store still refuses"
        );

        backend.down.store(false, Ordering::SeqCst);
        app.effect(&TotpEffect::RetrySave).unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(!frame.contains("isn't saved"), "{frame}");
        let retry = published(&mut app, Totp::KIND)
            .actions
            .into_iter()
            .find(|action| action.id == "retry-save")
            .unwrap();
        assert!(matches!(
            retry.contract.availability,
            Availability::Unavailable { .. }
        ));
        assert!(
            SecretStore::with_backend(TotpShell::ID, backend)
                .item("seed", &app.state()[0].id.to_string())
                .present_blocking()
        );
    }

    #[test]
    fn search_hits_open_the_account_they_name() {
        let (mut app, _host) = open(ui::secrets::memory_backend());
        add_through_the_form(&mut app, GITHUB);
        let account = app.state()[0].clone();
        let entries = Totp::search(&app.state());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "GitHub");
        let address = entries[0].action.clone().unwrap();
        assert_eq!(address.resource, account_resource_id(account.id));
        app.call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap();
    }

    #[test]
    fn captures_stage_demo_accounts_without_this_devices_secrets() {
        for scenario in SCENARIOS {
            let mut app = TestApp::<TotpShell>::capture(Some(scenario)).unwrap();
            let expected = if *scenario == "empty" {
                0
            } else {
                demo_accounts().len()
            };
            assert_eq!(app.state().len(), expected, "{scenario}");
            let frame = app.harness().inspection_json().unwrap();
            let shows = |text: &str| frame.contains(text);
            match *scenario {
                "add" | "add-picker" | "add-advanced" => {
                    assert!(shows("Secret key"), "{scenario}: {frame}");
                }
                "provision" => assert!(shows("Secret key"), "{scenario}: {frame}"),
                "persistence-error" => assert!(shows("Retry saving secrets"), "{frame}"),
                "missing-secret" => assert!(shows("Set up GitHub"), "{frame}"),
                "empty" => assert!(shows("No matching accounts"), "{frame}"),
                _ => assert!(shows("Copy code for GitHub"), "{scenario}: {frame}"),
            }
        }
        // A live window starts on this device's own, empty, vault.
        let mut live = TestApp::<TotpShell>::new().unwrap();
        assert!(live.state().is_empty());
        let frame = live.harness().inspection_json().unwrap();
        assert!(!frame.contains("GitHub"), "{frame}");
    }
}
