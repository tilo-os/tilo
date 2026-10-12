//! Authenticator as a resource: secret-free account metadata in the
//! document ([`Totp`]), each account's seed in this device's secret store,
//! and what only this device can do with a seed (show and copy codes, set
//! one up) as effects.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::icons::Icon;
use ui::search::SearchRequest;
use ui::secrets::SecretStore;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Effect, Id, Invoke, Refusal, SearchEntry, resource};
use ui::tokens::SpacingStep;
use ui::widget::{ButtonRole, Element, NavShell, Spacer};

use crate::app::{AuthenticatorApp, Notice};
pub use crate::model::Totp;
use crate::model::{Account, StoredAccount, demo_accounts};

#[resource(kind = "totp.vault", label = "Accounts", search = Totp::search)]
impl Totp {
    /// Move an account one place up.
    #[action]
    pub fn move_up(&mut self, account: &Id) -> Result<(), Refusal> {
        let at = self
            .accounts
            .position(account)
            .ok_or("there is no such account")?;
        let to = at.checked_sub(1).ok_or("this account is already first")?;
        self.accounts.move_to(account, to);
        Ok(())
    }

    /// Move an account one place down.
    #[action]
    pub fn move_down(&mut self, account: &Id) -> Result<(), Refusal> {
        let at = self
            .accounts
            .position(account)
            .ok_or("there is no such account")?;
        if at + 1 >= self.accounts.len() {
            return Err(Refusal::new("this account is already last"));
        }
        self.accounts.move_to(account, at + 1);
        Ok(())
    }

    /// Delete an account and forget its secret on every device.
    #[action(destructive)]
    pub fn delete(&mut self, account: &Id) -> Result<(), Refusal> {
        self.accounts
            .remove(account)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such account"))
    }
}

impl Totp {
    /// Each account, found by its issuer and name, opening it.
    fn search(&self) -> Vec<SearchEntry> {
        self.list()
            .iter()
            .map(|account| {
                let id = account_resource_id(account.id);
                SearchEntry::new(account.title(), account.label.clone()).opening(
                    id,
                    &TotpEffect::OpenAccount {
                        account: account.id.to_string(),
                    },
                )
            })
            .collect()
    }

    /// The insert that adds `account` at the end of the list.
    pub fn add(&self, account: &Account) -> Option<Invoke<Self>> {
        Invoke::edit(&Self::paths().accounts.insert_as(
            &Self::item(account.id),
            self.accounts.len(),
            &StoredAccount::of(account),
        ))
        .ok()
    }
}

/// The resource an account is published as: its item of the vault, which
/// this device's live row joins.
pub fn account_resource_id(id: crate::model::AccountId) -> String {
    tilo_resource::items::item_resource_id::<Totp>("accounts", &Totp::item(id))
}

/// One line naming an account, never its secret.
pub(crate) fn account_label(a: &Account) -> String {
    let issuer = safe_identity_field(&a.issuer, &a.secret);
    let label = safe_identity_field(&a.label, &a.secret);
    match (issuer.is_empty(), label.is_empty()) {
        (false, false) => format!("{issuer} — {label}"),
        (false, true) => issuer.to_string(),
        (true, false) => label.to_string(),
        (true, true) => "Account".to_string(),
    }
}

/// `value`, unless it contains the secret: what a label may say.
pub(crate) fn safe_identity_field<'a>(value: &'a str, secret: &str) -> &'a str {
    let value = value.trim();
    let secret = secret.trim();
    if !secret.is_empty()
        && value
            .to_ascii_uppercase()
            .contains(&secret.to_ascii_uppercase())
    {
        ""
    } else {
        value
    }
}

/// What the Authenticator does with what only this device holds: the add
/// form, an account's codes and its secret.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "totp.effect", device_only)]
pub enum TotpEffect {
    /// Add account.
    ///
    /// Opens the protected form on this device: a setup link carries the
    /// secret, so it is never an action argument.
    #[effect(kind = "read")]
    AddAccount,
    /// Open account.
    #[effect(kind = "read")]
    OpenAccount {
        #[param("Id of the account")]
        account: String,
    },
    /// Copy current code.
    #[effect(privileged, kind = "read")]
    CopyCode {
        #[param("Id of the account")]
        account: String,
    },
    /// Set up on this device.
    ///
    /// Opens the protected form for the account's secret on this device.
    #[effect(kind = "read")]
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

/// The capture scenarios: the list, and each state the app has a screen for.
pub const SCENARIOS: &[&str] = &[
    "list",
    "hover",
    "copied",
    "empty",
    "add",
    "add-advanced",
    "provision",
    "missing-secret",
    "persistence-error",
    "expiring",
];

/// Framework app: the navigation shell, whose header row carries the search
/// and add buttons, around the authenticator surface over [`Totp`].
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

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn view(cx: &mut ViewCx<Totp>) -> Element {
        // The header's actions are for a vault with accounts in it: with none,
        // the empty state asks for the first one and the header stays quiet.
        let has_accounts = cx.local(true);
        let count = cx.local(0usize);
        let notice = cx.local(None);
        let content = cx.surface(
            AuthenticatorApp::new()
                .with_account_signal(has_accounts.clone())
                .with_count_signal(count.clone())
                .with_notice_signal(notice.clone()),
        );
        let search = cx.switch(&has_accounts, |cx, has_accounts| -> Element {
            if *has_accounts {
                Box::new(cx.search_button(SearchRequest::this_app()))
            } else {
                Box::new(Spacer::token(SpacingStep::S0))
            }
        });
        let add = cx.switch(&has_accounts, |cx, has_accounts| -> Element {
            if *has_accounts {
                // The vault's own "Add account" effect, the one the palette
                // and agents invoke, which the surface answers by opening the
                // add sheet.
                Box::new(
                    cx.effect_button("Add account", &TotpEffect::AddAccount)
                        .with_icon(Icon::Plus)
                        .role(ButtonRole::Primary),
                )
            } else {
                Box::new(Spacer::token(SpacingStep::S0))
            }
        });
        Box::new(CountedShell {
            shell: NavShell::new()
                .content(content)
                .content_header("Accounts", vec![search, add]),
            count,
            notice,
        })
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
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
            .enumerate()
            .filter_map(|(at, account)| {
                Invoke::edit(&Totp::paths().accounts.insert_as(
                    &Totp::item(account.id),
                    at,
                    &StoredAccount::of(account),
                ))
                .ok()
            })
            .collect()
    }
}

/// The navigation shell whose header says how many accounts the vault holds,
/// and whose toast shows the surface's notices.
struct CountedShell {
    shell: NavShell,
    count: ui::LocalState<usize>,
    notice: ui::LocalState<Option<Notice>>,
}

impl ui::widget::Decorator for CountedShell {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let count = self.count.get();
        self.shell.set_content_count((count > 0).then_some(count));
        let changed = ui::widget::Widget::update(&mut self.shell, state, path);
        let Some(notice) = self.notice.with(Clone::clone) else {
            return changed;
        };
        self.notice.set(None);
        match notice {
            Notice::Show(text, kind) => self.shell.toast(text, kind),
            Notice::Clear => self.shell.dismiss_toast(),
        }
        true
    }
}

impl ResourceSurface<Totp, TotpEffect> for AuthenticatorApp {
    fn present(&mut self, state: &Rc<Totp>) {
        self.present_accounts(&Rc::new(state.list()));
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
            TotpEffect::OpenAccount {
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

    use crate::secrets::flaky::Flaky;

    const SECRET: &str = "JBSWY3DPEHPK3PXP";

    /// Add the GitHub account through the form.
    fn add_github(app: &mut TestApp<TotpShell>) {
        add_through_the_form(app, "GitHub", "ada@example.com", SECRET);
    }

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

    /// Run frames until a sheet that just opened or closed has settled.
    fn settle(app: &mut TestApp<TotpShell>) {
        for _ in 0..3 {
            app.harness().relayout();
        }
        app.settle();
    }

    /// Type `text` into the sheet's field `name`, after what it holds.
    fn type_into(app: &mut TestApp<TotpShell>, name: &str, text: &str) {
        app.harness()
            .click_named(name)
            .unwrap_or_else(|error| panic!("the sheet shows no {name}: {error}"));
        app.harness().type_text(text);
    }

    /// Save the open sheet with Enter, as a person does.
    fn save(app: &mut TestApp<TotpShell>) {
        app.harness().key("Enter");
        settle(app);
    }

    /// Add an account through the protected form, as a person does.
    fn add_through_the_form(app: &mut TestApp<TotpShell>, issuer: &str, label: &str, secret: &str) {
        app.effect(&TotpEffect::AddAccount).unwrap();
        settle(app);
        type_into(app, "totp-issuer", issuer);
        type_into(app, "totp-account-name", label);
        type_into(app, "totp-secret-key", secret);
        save(app);
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
        add_github(&mut app);

        let accounts = app.state().list();
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
        assert_eq!(row.kind, "totp.vault.accounts");
        let ids: Vec<&str> = row
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for want in [
            "move-up",
            "move-down",
            "delete",
            "open-account",
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
        let add = app.state().add(&remote).unwrap();
        app.invoke(add).unwrap();
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
        settle(&mut app);
        type_into(&mut app, "totp-secret-key", &secret);
        save(&mut app);

        let row = published(&mut app, &id);
        assert_eq!(
            row.presentation.as_ref().and_then(|p| p.badge.as_deref()),
            Some("Ready")
        );
        assert!(!row.actions.iter().any(|a| a.id == "set-up-local"));
        assert_eq!(
            app.state().list()[0].issuer,
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
        add_github(&mut app);
        add_through_the_form(&mut app, "AWS", "ops", "NB2W45DFOIZA");
        let [first, second] = [app.state().list()[0].id, app.state().list()[1].id];
        let row = account_resource_id(first);

        app.call_on(&row, "move-down", serde_json::json!({}))
            .unwrap();
        assert_eq!(
            app.state().list()[0].id,
            second,
            "the reorder reached the document"
        );
        assert!(app.undo());
        assert_eq!(app.state().list()[0].id, first, "and is one undo step");
        app.invoke(Totp::MOVE_DOWN.with(Totp::item(first))).unwrap();
        assert_eq!(
            app.state().list()[0].id,
            second,
            "the same verb invoked directly"
        );

        let delete = Totp::DELETE.with(Totp::item(first));
        assert!(
            matches!(
                app.invoke(delete.clone()),
                Err(tilo_actions::DoorError::ConfirmationRequired)
            ),
            "deleting an account asks first, however it is reached"
        );
        assert_eq!(app.state().list().len(), 2);
        app.invoke_confirmed(delete).unwrap();
        app.settle();
        assert_eq!(
            app.state().list().len(),
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
        add_github(&mut app);
        let id = app.state().list()[0].id;
        let account = Totp::paths().accounts.item(&Totp::item(id));
        app.invoke(Invoke::edit(&account.color.set(5)).unwrap())
            .unwrap();
        let mut peer = app.peer_document();
        account
            .label
            .replace("ada@corp.example")
            .apply_local(&mut peer)
            .unwrap();

        click(&mut app, &format!("totp.select:{id}"));
        app.harness().set_modifiers(&["ctrl".to_owned()]);
        app.harness().key("e");
        app.harness().set_modifiers(&[]);
        settle(&mut app);
        app.harness()
            .click_named("totp-issuer")
            .expect("the sheet shows the issuer");
        app.harness()
            .perform_named(
                "totp-issuer",
                &ui::a11y::Action::SetValue("GitHub Work".into()),
            )
            .expect("the issuer is a field");
        save(&mut app);
        app.merge_peer(&mut peer).unwrap();
        app.settle();

        let account = &app.state().list()[0];
        assert_eq!(
            account.issuer, "GitHub Work",
            "the edit reached the document"
        );
        assert_eq!(
            account.label, "ada@corp.example",
            "the collaborator's label stands"
        );
        assert_eq!(account.color, 5, "the edit keeps the account's colour");
        assert!(app.undo(), "the edit is one undo step");
        assert_eq!(app.state().list()[0].issuer, "GitHub");
    }

    #[test]
    fn a_refused_secret_is_reported_and_saved_by_a_retry_the_user_asks_for() {
        let backend = Flaky::new();
        let (mut app, _host) = open(backend.clone());
        backend.down.store(true, Ordering::SeqCst);
        add_github(&mut app);
        assert_eq!(app.state().list().len(), 1, "the account is shared anyway");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Retry saving keys"), "{frame}");
        assert!(
            app.effect(&TotpEffect::RetrySave).is_err(),
            "the store still refuses"
        );

        backend.down.store(false, Ordering::SeqCst);
        app.effect(&TotpEffect::RetrySave).unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(!frame.contains("Retry saving keys"), "{frame}");
        let retry = published(&mut app, <Totp as ui::tilo_resource::Resource>::KIND)
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
                .item("seed", &app.state().list()[0].id.to_string())
                .present_blocking()
        );
    }

    /// What the window's accessibility tree names, by role.
    fn names(app: &mut TestApp<TotpShell>) -> Vec<(String, String)> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .map(|(_, node)| {
                (
                    format!("{:?}", node.role()),
                    node.label().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn the_header_opens_the_sheet_and_enter_copies_the_selected_code() {
        let (mut app, _host) = open(ui::secrets::memory_backend());
        add_github(&mut app);

        let listed = names(&mut app);
        assert!(
            listed
                .iter()
                .any(|(role, name)| role == "ListItem" && name.starts_with("GitHub")),
            "the account is listed: {listed:?}"
        );
        assert!(
            listed
                .iter()
                .any(|(role, name)| role == "Button" && name == "Add account"),
            "the header carries the add button"
        );

        // The header's add button opens the sheet.
        app.harness()
            .click_named("add-account")
            .expect("the header shows the add button");
        settle(&mut app);
        let opened = names(&mut app);
        assert!(
            opened
                .iter()
                .any(|(role, name)| role == "Dialog" && name == "Add account"),
            "pressing it opens the sheet: {opened:?}"
        );

        // A click puts the caret in a field, Tab moves it on.
        type_into(&mut app, "totp-issuer", "Acme");
        app.harness().tab(false);
        let after_tab = app.harness().a11y_focus();
        let focus_order = app.harness().focus_access_nodes();
        app.harness().type_text("bob");
        let typed: Vec<_> = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| format!("{:?}", node.role()) == "TextInput")
            .map(|(_, node)| node.value().unwrap_or_default().to_owned())
            .collect();
        assert!(
            typed.iter().any(|value| value == "Acme") && typed.iter().any(|value| value == "bob"),
            "the click and Tab each put the caret in a field: {typed:?}; after Tab {after_tab:?}; order {focus_order:?}"
        );

        // Escape closes the sheet and Enter copies the selected code.
        app.harness().key("Escape");
        settle(&mut app);
        assert!(
            !names(&mut app)
                .iter()
                .any(|(role, name)| role == "Dialog" && name == "Add account"),
            "Escape closes it"
        );
        assert_eq!(app.state().list().len(), 1, "nothing was added");
        app.harness().key("Enter");
        app.harness().relayout();
        let copied: Vec<_> = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.value().map(str::to_owned))
            .collect();
        assert!(
            copied.iter().any(|value| value == "Copied GitHub code"),
            "Enter copies the selected account and says so: {copied:?}"
        );
    }

    #[test]
    fn search_hits_open_the_account_they_name() {
        let (mut app, _host) = open(ui::secrets::memory_backend());
        add_github(&mut app);
        let account = app.state().list()[0].clone();
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
            assert_eq!(app.state().list().len(), expected, "{scenario}");
            let frame = app.harness().inspection_json().unwrap();
            let shows = |text: &str| frame.contains(text);
            match *scenario {
                "add" | "add-advanced" | "provision" => {
                    assert!(shows("Secret key"), "{scenario}: {frame}");
                }
                "persistence-error" => assert!(shows("Retry saving keys"), "{frame}"),
                "missing-secret" => assert!(shows("Set up GitHub"), "{frame}"),
                "empty" => assert!(!shows("Copy code for"), "{frame}"),
                _ => assert!(shows("Copy code for GitHub"), "{scenario}: {frame}"),
            }
        }
        // A live window starts on this device's own, empty, vault.
        let mut live = TestApp::<TotpShell>::new().unwrap();
        assert!(live.state().list().is_empty());
        let frame = live.harness().inspection_json().unwrap();
        assert!(!frame.contains("GitHub"), "{frame}");
    }

    fn replica() -> ui::tilo_resource::Replica<Totp> {
        let mut replica = ui::tilo_resource::Replica::<Totp>::new();
        for account in demo_accounts() {
            let add = replica.state().unwrap().add(&account).unwrap();
            replica.apply(add).unwrap();
        }
        replica
    }

    /// The shared accounts carry their metadata and never a secret; two
    /// devices adding and editing at once converge, and a deletion wins
    /// over a concurrent edit of the account.
    #[test]
    fn the_vault_merges_metadata_without_secrets_or_resurrection() {
        let mut ours = replica();
        let listed = ours.state().unwrap().list();
        assert_eq!(listed.len(), demo_accounts().len());
        assert!(listed.iter().all(|account| account.secret.is_empty()));
        assert!(
            demo_accounts()
                .iter()
                .all(|account| !holds(&ours.save(), &account.secret)),
            "no secret is in the document"
        );

        let first = Totp::item(listed[0].id);
        let mut theirs = ours.fork();
        ours.apply_confirmed(Totp::DELETE.with(first.clone()))
            .unwrap();
        theirs
            .apply(
                Invoke::edit(
                    &Totp::paths()
                        .accounts
                        .item(&first)
                        .label
                        .replace("late edit"),
                )
                .unwrap(),
            )
            .unwrap();
        let mut extra = demo_accounts().remove(1);
        extra.id = crate::model::AccountId::from_u128(42);
        let add = theirs.state().unwrap().add(&extra).unwrap();
        theirs.apply(add).unwrap();
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        let merged = ours.state().unwrap().list();
        assert!(merged.iter().all(|account| account.id != listed[0].id));
        assert!(merged.iter().any(|account| account.id == extra.id));
    }

    /// An agent recolours an account in one call; codes the authenticator
    /// cannot compute are refused; history names the account.
    #[test]
    fn edits_are_checked_and_history_names_the_account() {
        let (mut app, _host) = open(ui::secrets::memory_backend());
        add_github(&mut app);
        let id = Totp::item(app.state().list()[0].id);
        let account = Totp::paths().accounts.item(&id);
        app.call(
            "field-edit",
            serde_json::json!({ "edit": account.digits.set(8) }),
        )
        .unwrap();
        assert!(
            app.invoke(Invoke::edit(&account.period.set(0)).unwrap())
                .is_err(),
            "a code changes at most every 300 seconds and at least every 5"
        );
        app.call_on(
            &tilo_resource::items::item_resource_id::<Totp>("accounts", &id),
            "delete",
            serde_json::json!({}),
        )
        .expect_err("deleting asks first");
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences,
            ["Added an account", "GitHub now shows 8-digit codes"]
        );
    }
}
