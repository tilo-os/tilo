//! The vault's rows and effects, published as a framework app.

use ui::framework::{EffectStatus, Effected, Fixture, Publishes, ResourceApp, RunMode, ViewCx};
use ui::widget::Element;

use crate::actions::PasswordsEffect;
use crate::window::PasswordsWindow;

#[cfg(test)]
#[path = "credential_owner_tests.rs"]
mod credential_owner_tests;

/// The vault's published document, with its entries sealed by the workspace document keys.
pub use passwords_model::Passwords;

pub struct DemoVault;

pub struct PasswordsShell;

impl ResourceApp for PasswordsShell {
    type Resource = Passwords;
    const ID: &'static str = ui::apps::PASSWORDS;
    const TITLE: &'static str = "Passwords";
    const TITLE_KEY: &'static str = "passwords-app-title";
    const ICON: Option<&'static str> = Some("passwords");
    const WORKSPACE_RESOURCE: bool = true;
    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        capture_protected: true,
        ..ui::application::ComponentBehaviour::DEFAULT
    };

    fn view(cx: &mut ViewCx<Passwords>) -> Element {
        let live = cx.mode().is_live();
        let owner = live
            .then(|| cx.take_lent::<accounts::CredentialOwner>())
            .flatten();
        let matches_device = |owner: &accounts::CredentialOwner| {
            cx.me()
                .and_then(ui::tilo_resource::MemberId::as_person)
                .is_some_and(|me| me.as_str() == owner.actor())
                && cx.device() == Some(owner.device())
        };
        // The device's own vault, when the session lent it for this person on this device.
        let device_owner = owner.filter(matches_device);
        let in_workspace = live && cx.workspace().is_some();
        // With no lent owner, a window of its own may still read this device's store.
        let local_device_vault = in_workspace && !cx.is_embedded() && device_owner.is_none();
        let can_import_device_vault = device_owner.is_some() || local_device_vault;
        let window = if cx.mode() == RunMode::Past {
            PasswordsWindow::new(crate::PasswordsModel::closed())
        } else if cx.fixture::<DemoVault>().is_some() {
            // The fixture stands in for the entries the window reads, not for the workspace's
            // vault document it reads them from: that is opened as a workspace's window opens it.
            let _ = cx
                .resource_opener::<passwords_model::WorkspaceVaultEntries>(Self::ID)
                .open(crate::workspace_vault::WORKSPACE_VAULT_DOCUMENT);
            PasswordsWindow::demo()
        } else if in_workspace {
            PasswordsWindow::new(crate::PasswordsModel::workspace(
                cx.resource_opener(Self::ID),
                device_owner,
                local_device_vault,
            ))
        } else {
            PasswordsWindow::new(crate::PasswordsModel::closed())
        };
        cx.on_open(window.opener());
        cx.on_stage(window.stager());
        window.wake_through(cx.waker());
        answer_effects(cx, &window.vault(), can_import_device_vault);
        window.vault().borrow_mut().jobs = Some(cx.window_jobs::<PasswordsWindow>());
        let vault = window.vault();
        let (rows, row, entries, unlocked) = (vault.clone(), vault.clone(), vault.clone(), vault);
        cx.publishing(
            Publishes::new()
                .live_rows(move || rows.borrow().item_rows())
                .live_row(move |id| row.borrow().item_row(id))
                .search_entries(move || crate::search::entries(entries.borrow().published_items()))
                .secrets_unlocked(move || unlocked.borrow().secrets_unlocked()),
            Box::new(window),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        crate::window::SCENARIOS
    }

    /// A screenshot never opens the person's vault: it shows synthetic items with no store
    /// behind them.
    fn fixture(fixture: &mut Fixture<Passwords>) {
        fixture.provide(DemoVault);
    }
}

/// Answer what the palette, an agent, a script or a search hit asks of the vault; the
/// window brings itself in step on its next sync.
fn answer_effects(cx: &ViewCx<Passwords>, vault: &crate::vault::Shared, can_import: bool) {
    let (answer, status) = (vault.clone(), vault.clone());
    cx.effects(move |effect: PasswordsEffect, _cx| {
        if matches!(effect, PasswordsEffect::ImportDeviceVault) {
            if !can_import {
                return Err("the device vault cannot be imported here".into());
            }
            let imported = answer.borrow_mut().convert_device_vault()?;
            return Ok(Effected::value(serde_json::json!({ "imported": imported })));
        }
        let listing = matches!(effect, PasswordsEffect::ListLogins);
        let mut vault = answer.borrow_mut();
        vault.perform(effect)?;
        Ok(if listing {
            Effected::value(vault.logins())
        } else {
            Effected::done()
        })
    })
    .status(move || {
        let mut unavailable = Vec::new();
        if !can_import {
            unavailable.push(EffectStatus::unavailable(
                PasswordsEffect::IMPORT_DEVICE_VAULT,
                "no_device_vault",
                "The device vault is unavailable here",
            ));
        }
        if !status.borrow().is_editing() {
            let reason = "Open an item in the editor first";
            unavailable.extend([
                EffectStatus::unavailable(PasswordsEffect::SAVE, "not_editing", reason),
                EffectStatus::unavailable(PasswordsEffect::CANCEL, "not_editing", reason),
                EffectStatus::unavailable(PasswordsEffect::GENERATE, "not_editing", reason),
            ]);
        }
        unavailable
    });
}

#[cfg(test)]
mod tests {
    use accounts::SecretItem;
    use ui::prelude::TestApp;
    use ui::tilo_actions::{self, ConfirmationPolicy, OperationKind};
    use zeroize::Zeroizing;

    use super::*;
    use crate::actions::item_resource_id;
    use ui::tilo_resource::Resource;

    /// The device's own store keeps the secrets that belong to this device alone: the
    /// scope the messaging profile key carries, and the networks this machine joined.
    const MESSAGING_PROFILE_SCOPE: &str = "tilo.messaging.profile.scope";

    /// Opening Passwords in a workspace converts what the device vault may share into the
    /// workspace document, once, and leaves what belongs to the device where it is.
    #[test]
    fn a_workspace_window_converts_the_device_vaults_shareable_logins_once() {
        let _device = crate::test_vault::isolated();
        let mut store = accounts::SecretStore::open();
        let mut login = SecretItem::login("Old device login", "ada", "shareable-7X");
        store.put(&mut login).unwrap();
        let login_id = login.id.clone();
        let mut wifi = SecretItem::login("Home Wi-Fi", "ada", "wifi-kept-7X");
        wifi.kind = accounts::SecretItemKind::Wifi;
        store.put(&mut wifi).unwrap();
        let wifi_id = wifi.id.clone();
        let mut auth = SecretItem::login("Messages account encryption key", "", "auth-kept-7X");
        auth.kind = accounts::SecretItemKind::Other;
        auth.fields
            .insert(MESSAGING_PROFILE_SCOPE.to_owned(), "{}".to_owned());
        assert!(
            !auth.may_share_with_workspace(),
            "device authentication material never leaves the device"
        );
        store.put(&mut auth).unwrap();
        let auth_id = auth.id.clone();
        drop(store);

        let mut app = crate::test_vault::workspace_window().unwrap();
        let items = crate::test_vault::stored(&mut app);
        assert_eq!(
            items.len(),
            1,
            "the workspace holds the shareable login alone"
        );
        assert_eq!(items[0].id, login_id);
        let revealed = app
            .surface_mut::<PasswordsWindow>()
            .core()
            .model
            .as_ref()
            .unwrap()
            .reveal(&login_id)
            .unwrap();
        assert_eq!(
            revealed.as_str(),
            "shareable-7X",
            "sealed into the document"
        );

        let device = accounts::SecretStore::open().list();
        assert!(
            device.iter().all(|item| item.id != login_id),
            "the converted login left the device vault"
        );
        assert!(
            device.iter().any(|item| item.id == wifi_id),
            "the Wi-Fi network stayed"
        );
        assert!(
            device.iter().any(|item| item.id == auth_id),
            "the profile key stayed"
        );

        let mut again = app.reopen().unwrap();
        let answered = again
            .effect_confirmed(&PasswordsEffect::ImportDeviceVault)
            .expect("the effect answers");
        let value = answered
            .value
            .expect("the conversion says how many it imported");
        assert_eq!(
            value.get("imported").and_then(serde_json::Value::as_u64),
            Some(0),
            "opening it again converts nothing twice"
        );
        assert_eq!(crate::test_vault::stored(&mut again).len(), 1);
        assert_eq!(accounts::SecretStore::open().list().len(), device.len());
    }

    tilo_test_env::scenario! {
        fn the_passwords_shell_guards_its_vault_and_changes_it_by_action;

        phase every_launch_withholds_the_vault_window_from_captures;
        {
            const { assert!(<PasswordsShell as ResourceApp>::BEHAVIOUR.capture_protected) };
        }

        phase vault_items_are_live_rows_whose_effects_reach_the_vault;
        {
            let clipboard = std::sync::Arc::new(Clipboard::default());
            let _host = ui::process::enter(
                &ui::process::ProcessServices::null().with_clipboard(std::sync::Arc::clone(&clipboard) as _),
            );
            let mut app = crate::test_vault::personal_window().unwrap();
            let mut item = SecretItem::login("Private bank", "ada@private.example", "not-public-7A");
            item.add_origin("https://private.example").unwrap();
            let id = crate::test_vault::seed(&mut app, &mut item);
            let row_id = item_resource_id(&id.0);
            let row = published(&mut app, &row_id).expect("the item is a live row");
            assert_eq!(row.label, "Saved login");
            for action in &row.actions {
                assert_eq!(action.kind, OperationKind::Effect, "{}", action.id);
            }
            let vault = published(&mut app, Passwords::KIND).unwrap();
            let published_text = serde_json::to_string(&app.agent_surface()).unwrap();
            for private in ["Private bank", "ada@private.example", "not-public-7A"] {
                assert!(!published_text.contains(private), "published {private:?}");
            }
            assert!(
                vault
                    .actions
                    .iter()
                    .find(|action| action.id == "open")
                    .is_some_and(|open| open.params().iter().any(|p| p.name == "anchor")),
                "the vault opens an item by its anchor; the row opens itself"
            );
            for id in ["add", "import", "export", "filter"] {
                assert!(
                    vault.actions.iter().any(|action| action.id == id),
                    "the vault offers {id}"
                );
            }
            let save = vault
                .actions
                .iter()
                .find(|action| action.id == "save")
                .unwrap();
            assert!(
                matches!(
                    save.contract.availability,
                    ui::tilo_actions::Availability::Unavailable { .. }
                ),
                "nothing to save before an item is opened"
            );

            let copy = row
                .actions
                .iter()
                .find(|action| action.id == "copy")
                .unwrap();
            assert_eq!(copy.contract.confirmation, ConfirmationPolicy::Confirm);
            app.effect_confirmed(&PasswordsEffect::Copy { item: id.0.clone() })
                .unwrap();
            assert_eq!(
                clipboard.0.lock().unwrap().as_deref(),
                Some("not-public-7A")
            );
            assert!(
                clipboard.1.load(std::sync::atomic::Ordering::Acquire),
                "a password is copied as a secret, out of history and device sync"
            );

            let versions = app.versions().len();
            app.effect_on(&row_id, &PasswordsEffect::Open { item: id.0.clone() })
                .unwrap();
            app.effect(&PasswordsEffect::Filter {
                filter: "favorites".into(),
            })
            .unwrap();
            assert!(
                app.effect(&PasswordsEffect::Filter {
                    filter: "everything".into()
                })
                .is_err()
            );
            assert_eq!(app.versions().len(), versions);

            assert!(
                app.effect(&PasswordsEffect::Delete { item: id.0.clone() })
                    .is_err(),
                "deleting needs the caller's confirmation"
            );
            assert_eq!(crate::test_vault::stored(&mut app).len(), 1);
            app.effect_confirmed(&PasswordsEffect::Delete { item: id.0 })
                .unwrap();
            assert!(crate::test_vault::stored(&mut app).is_empty());
            assert!(published(&mut app, &row_id).is_none(), "its row is gone");
        }

        phase opening_an_anchor_shows_its_item_and_refuses_one_the_vault_lacks;
        // Opening an item's anchor, as a search hit or an agent does, shows it even where a
        // filter hid it; an item the vault lacks is refused.
        {
            let mut app = crate::test_vault::personal_window().unwrap();
            let mut item = SecretItem::login("Private bank", "ada@private.example", "not-public-7A");
            let id = crate::test_vault::seed(&mut app, &mut item);
            app.effect(&PasswordsEffect::Filter {
                filter: "favorites".into(),
            })
            .unwrap();
            app.call(
                "open",
                serde_json::json!({ "anchor": item_resource_id(&id.0) }),
            )
            .unwrap();
            assert!(
                app.call(
                    "open",
                    serde_json::json!({ "anchor": item_resource_id("not-in-the-vault") })
                )
                .is_err(),
                "an item the vault does not hold is not opened"
            );
            app.settle();
            assert_eq!(
                app.surface_mut::<PasswordsWindow>()
                    .core()
                    .selected
                    .as_ref(),
                Some(&id),
                "opening its anchor shows the item, as a search hit does"
            );
        }

        phase an_item_is_made_a_favorite_and_back_by_its_action;
        // A favorite is vault state an agent, a script or a screen reader sets by the item's
        // id, not only by the row's star.
        {
            let mut app = crate::test_vault::personal_window().unwrap();
            let mut item = SecretItem::login("Mail", "ada@example.com", "not-public-7A");
            let id = crate::test_vault::seed(&mut app, &mut item);
            let vault = published(&mut app, Passwords::KIND).unwrap();
            assert!(
                vault.actions.iter().any(|action| action.id == "favorite"),
                "the vault offers favorite"
            );
            fn stored(app: &mut TestApp<PasswordsShell>) -> bool {
                crate::test_vault::stored(app)
                    .iter()
                    .any(|item| item.tags.contains("favorite"))
            }
            let favorite = |favorite| PasswordsEffect::Favorite {
                item: id.0.clone(),
                favorite,
            };

            app.effect(&favorite(true)).unwrap();
            assert!(stored(&mut app), "the vault keeps the favorite");
            app.effect(&favorite(true)).unwrap();
            assert!(stored(&mut app), "asking again leaves it a favorite");
            app.effect(&PasswordsEffect::Filter {
                filter: "favorites".into(),
            })
            .unwrap();
            assert_eq!(
                app.surface_mut::<PasswordsWindow>().core().visible().len(),
                1,
                "Favorites lists it"
            );
            app.effect(&favorite(false)).unwrap();
            assert!(!stored(&mut app), "and no longer once it is removed");
            assert!(
                app.effect(&PasswordsEffect::Favorite {
                    item: "not-in-the-vault".into(),
                    favorite: true,
                })
                .is_err(),
                "an item the vault does not hold is refused"
            );
        }

        phase an_item_saved_from_the_editor_lands_in_the_vault;
        {
            let mut app = crate::test_vault::personal_window().unwrap();
            app.effect(&PasswordsEffect::Add).unwrap();
            app.harness()
                .click_named("passwords.field.name")
                .expect("the editor shows the name field");
            app.harness().type_text("Mail");
            app.effect(&PasswordsEffect::Generate).unwrap();
            app.effect(&PasswordsEffect::Save).unwrap();
            let items = crate::test_vault::stored(&mut app);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].title, "Mail");
            assert!(items[0].password.len() >= 20, "a generated password");
            assert!(
                app.effect(&PasswordsEffect::Save).is_err(),
                "the editor closed with the save"
            );
        }

        phase a_login_saved_in_one_step_lands_in_the_vault_and_its_password_nowhere_else;
        // A login an agent or a script saves in one step lands in the vault and is listed by
        // its name; the password it was given is in the vault alone: the action takes it as a
        // secret, and it is in none of what Passwords publishes, answers or a log of the call
        // would print.
        {
            use ui::tilo_actions::{ActionArgs, ParamType};
            const PASSWORD: &str = "secret-sentinel-41xq";
            let mut app = crate::test_vault::personal_window().unwrap();
            let vault = published(&mut app, Passwords::KIND).unwrap();
            let save = vault
                .actions
                .iter()
                .find(|action| action.id == "save-login")
                .expect("the vault offers save-login");
            assert_eq!(
                save.params()
                    .iter()
                    .find(|param| param.name == "password")
                    .map(|param| param.ty),
                Some(ParamType::Secret),
                "the password is a secret parameter"
            );
            let arguments = serde_json::json!({
                "name": "Mail",
                "origin": "https://mail.example",
                "username": "ada@mail.example",
                "password": PASSWORD,
            });
            let bound = ActionArgs::bind_json(save.params(), arguments.as_object().unwrap()).unwrap();
            assert!(
                !format!("{bound:?}").contains(PASSWORD),
                "bound arguments print the password redacted"
            );

            let answered = app.call("save-login", arguments).unwrap();
            assert!(!format!("{answered:?}").contains(PASSWORD));
            let items = crate::test_vault::stored(&mut app);
            assert_eq!(items.len(), 1, "the login is in the vault");
            assert_eq!(items[0].title, "Mail");
            assert_eq!(items[0].kind, accounts::SecretItemKind::Login);
            assert_eq!(items[0].password.as_str(), PASSWORD);
            assert!(
                app.effect(&PasswordsEffect::Save).is_err(),
                "no editor is left open behind it"
            );

            let listed = app
                .call("list-logins", serde_json::json!({}))
                .unwrap()
                .expect("the logins are answered");
            let listed = listed.to_string();
            assert!(listed.contains("Mail") && listed.contains(&items[0].id.0));
            for private in [PASSWORD, "ada@mail.example", "mail.example"] {
                assert!(!listed.contains(private), "the listing holds {private:?}");
            }
            let published_text = serde_json::to_string(&app.agent_surface()).unwrap();
            assert!(!published_text.contains(PASSWORD), "published the password");

            assert!(
                app.call(
                    "save-login",
                    serde_json::json!({"name": "", "origin": "", "username": "", "password": "x"})
                )
                .is_err(),
                "a login with no name is refused as the editor refuses it"
            );
            assert_eq!(crate::test_vault::stored(&mut app).len(), 1);
        }
    }

    /// A clipboard the test reads back, and whether it holds a secret.
    #[derive(Default)]
    struct Clipboard(
        std::sync::Mutex<Option<String>>,
        std::sync::atomic::AtomicBool,
    );

    impl ui::clipboard::ClipboardBackend for Clipboard {
        fn set_text(&self, _target: ui::clipboard::ClipboardTarget, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.into());
            self.1.store(false, std::sync::atomic::Ordering::Release);
            true
        }
        fn set_secret_text(&self, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.into());
            self.1.store(true, std::sync::atomic::Ordering::Release);
            true
        }
        fn text(&self, _target: ui::clipboard::ClipboardTarget) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn name(&self) -> &'static str {
            "passwords-test-clipboard"
        }
    }

    /// Whether the window shows `name` selected.
    fn selected(node: &ui::headless::InspectionNode, name: &str) -> bool {
        fn find(node: &ui::a11y::AccessNode, name: &str) -> bool {
            (node.name.as_deref() == Some(name) && node.state.selected)
                || node.children.iter().any(|child| find(child, name))
        }
        node.semantics
            .as_ref()
            .is_some_and(|semantics| find(semantics, name))
            || node.children.iter().any(|child| selected(child, name))
    }

    fn published(
        app: &mut TestApp<PasswordsShell>,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        fn find(
            resources: &[tilo_actions::ResourceSpec],
            id: &str,
        ) -> Option<tilo_actions::ResourceSpec> {
            resources.iter().find_map(|resource| {
                if resource.id.as_str() == id {
                    Some(resource.clone())
                } else {
                    find(&resource.children, id)
                }
            })
        }
        find(&app.harness().resources(), id)
    }

    /// Export and backup ask where the file goes, then write it there readable by this
    /// person only, named for what it holds.
    #[test]
    fn export_and_backup_write_private_files_where_the_person_chose() {
        use crate::vault::{Dialog, Request};
        let out = tempfile::tempdir().unwrap();
        let (services, recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        let mut app = crate::test_vault::personal_window().unwrap();
        let mut login = SecretItem::login("Example", "ada", "export-secret");
        login.add_origin("https://example.com").unwrap();
        crate::test_vault::seed(&mut app, &mut login);
        let ask = |app: &mut TestApp<PasswordsShell>, request: Request| {
            app.surface::<PasswordsWindow, _>(|window| {
                window.requests.borrow_mut().push(request);
                window.settle();
            })
            .unwrap();
            app.settle();
        };
        let private = |path: &std::path::Path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        };

        ask(&mut app, Request::Export);
        ask(&mut app, Request::ConfirmExport);
        let reminded = app
            .surface::<PasswordsWindow, _>(|window| {
                matches!(
                    window.core().dialog,
                    Some(Dialog::ExportPlaintext { reminded: true, .. })
                )
            })
            .unwrap();
        assert!(reminded, "a plaintext export needs the acknowledgement");
        assert!(recorder.pick_requests().is_empty());
        ask(&mut app, Request::Acknowledge(true));
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(out.path().join("export")));
        ask(&mut app, Request::ConfirmExport);
        assert_eq!(
            recorder.pick_requests()[0].title(),
            "Export passwords as CSV"
        );
        let written = out.path().join("export.csv");
        let rows =
            crate::transfer::parse_browser_csv(std::fs::File::open(&written).unwrap()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].password.as_str(), "export-secret");
        private(&written);

        ask(&mut app, Request::Backup);
        app.surface::<PasswordsWindow, _>(|window| {
            let mut vault = window.core_mut();
            vault.backup_password = Zeroizing::new("a memorable backup password".into());
            vault.backup_confirmation = Zeroizing::new("a memorable backup password".into());
        })
        .unwrap();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(out.path().join("vault")));
        ask(&mut app, Request::ConfirmBackup);
        assert_eq!(
            recorder.pick_requests()[1].title(),
            "Back up password vault"
        );
        let backup = out.path().join("vault.tilopw");
        let items = crate::transfer::import_encrypted_backup(
            &std::fs::read(&backup).unwrap(),
            "a memorable backup password",
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        private(&backup);
        let cleared = app
            .surface::<PasswordsWindow, _>(|window| window.core().backup_password.is_empty())
            .unwrap();
        assert!(cleared, "the backup password is not kept");
    }

    #[test]
    fn captures_show_a_demo_vault_of_their_own() {
        let mut person = crate::test_vault::workspace_window().unwrap();
        for scenario in crate::window::SCENARIOS {
            let mut app = TestApp::<PasswordsShell>::capture(Some(scenario)).unwrap();
            let root = app.harness().inspection().root;
            let frame = app.harness().inspection_json().unwrap();
            for secret in ["Harbor-Lights-2019", "harbourlights"] {
                assert!(!frame.contains(secret), "{scenario}: {frame}");
            }
            let shows = |text: &str| frame.contains(text);
            match *scenario {
                "editor" => assert!(shows("Northstar Bank") && shows("Save"), "{frame}"),
                "delete" => assert!(shows("Delete item"), "{frame}"),
                "review" => assert!(selected(&root, "Password review"), "{frame}"),
                "app-access" => assert!(selected(&root, "App access"), "{frame}"),
                "transfer" | "import" | "export" | "backup" => {
                    assert!(selected(&root, "Import and export"), "{scenario}: {frame}");
                }
                "api-key" => assert!(
                    shows("Cloud console") && shows("lovelace-prod-0Il1O"),
                    "{frame}"
                ),
                "empty" | "new-item" => {}
                _ => assert!(shows("Northstar Bank"), "{scenario}: {frame}"),
            }
        }
        assert!(
            crate::test_vault::stored(&mut person).is_empty(),
            "no capture wrote to the person's vault"
        );
    }
}

ui::reach_tests!(PasswordsShell);

#[cfg(test)]
mod window_bar_tests {
    /// The window bar's sidebar button flips the sidebar on every click, in a window wide
    /// enough to dock it and in a narrow one.
    #[test]
    fn the_window_bars_sidebar_button_flips_the_sidebar() {
        let _vault = crate::test_vault::isolated();
        ui::framework::testing::check_window_sidebar_button(|| {
            crate::test_vault::personal_window()
        })
        .unwrap_or_else(|error| panic!("{error}"));
    }
}

#[cfg(test)]
mod reveal_tests {
    use accounts::SecretItem;
    use ui::prelude::TestApp;

    use crate::window::PasswordsWindow;

    /// A person who presses the eye of a password and confirms sees the password, in the
    /// window they pressed it in.
    #[test]
    fn a_confirmed_reveal_shows_the_password() {
        let mut app = crate::test_vault::personal_window().unwrap();
        let mut item = SecretItem::login("Bakery", "ada", "crumb-Loaf-42");
        let id = crate::test_vault::seed(&mut app, &mut item);
        assert!(
            app.surface_mut::<PasswordsWindow>().core_mut().open(&id),
            "the saved login is listed"
        );
        app.settle();
        press(&mut app, "Show password Password");
        press(&mut app, "Reveal secret");
        assert_eq!(
            app.surface_mut::<PasswordsWindow>()
                .core()
                .revealed
                .as_ref()
                .map(|(_, value, _)| value.to_string()),
            Some("crumb-Loaf-42".to_owned()),
            "the confirmed reveal shows the password"
        );
    }

    /// Click the middle of the control a person hears as `name`, as a pointer does.
    fn press(app: &mut TestApp<super::PasswordsShell>, name: &str) {
        let bounds = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(name) && !node.is_hidden())
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("no control reads out as {name:?}"));
        app.harness()
            .click_at(bounds.x0.midpoint(bounds.x1), bounds.y0.midpoint(bounds.y1));
        app.settle();
    }
}
