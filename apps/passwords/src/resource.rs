//! Passwords as a framework app. The vault is this device's encrypted
//! credential store, not a document: its items are published as live rows,
//! and everything Passwords does with them is a [`PasswordsEffect`]. The
//! resource's model holds nothing, so no secret can sync; the place shown
//! and the open item are this window's own view state.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface,
    RunMode, ViewCx,
};
use ui::tilo_resource::{self, Model, resource};
use ui::widget::Element;

use crate::actions::PasswordsEffect;
use crate::window::PasswordsSurface;

#[cfg(test)]
#[path = "credential_owner_tests.rs"]
mod credential_owner_tests;

/// The password vault resource: the door the vault's rows and effects are
/// published on. It stores nothing; the vault is the device's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Passwords {}

#[resource(kind = "passwords.vault", label = "Password vault")]
impl Passwords {}

/// A capture shows the fixture vault in place of the person's.
pub struct DemoVault;

/// Framework app: the passwords surface over [`Passwords`].
pub struct PasswordsShell;

impl ResourceApp for PasswordsShell {
    type Resource = Passwords;
    const ID: &'static str = "so.tilo.passwords";
    const TITLE: &'static str = "Passwords";
    const ICON: Option<&'static str> = Some("passwords");
    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }
    // The window shows secrets: captures, recordings and remote viewers get
    // a placeholder in every launch.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        capture_protected: true,
        ..ui::application::ComponentBehaviour::DEFAULT
    };

    fn view(cx: &mut ViewCx<Passwords>) -> Element {
        let surface = if cx.mode() == RunMode::Past {
            PasswordsSurface::new(crate::PasswordsModel::closed())
        } else if cx.fixture::<DemoVault>().is_some() {
            PasswordsSurface::demo()
        } else if let Some(owner) = cx.take_lent::<accounts::CredentialOwner>() {
            let model = if cx.me() == Some(owner.principal()) && cx.device() == Some(owner.device())
            {
                crate::PasswordsModel::shared(owner)
            } else {
                crate::PasswordsModel::closed()
            };
            PasswordsSurface::new(model)
        } else {
            PasswordsSurface::new(crate::PasswordsModel::open())
        };
        cx.on_open(surface.opener());
        cx.surface(surface)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::window::SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    /// A screenshot never opens the person's vault: it shows synthetic items
    /// with no store behind them.
    fn fixture(fixture: &mut Fixture<Passwords>) {
        fixture.provide(DemoVault);
    }
}

impl ResourceSurface<Passwords, PasswordsEffect> for PasswordsSurface {
    fn present(&mut self, _state: &Rc<Passwords>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: PasswordsEffect,
        _cx: &mut EffectCx<Passwords, Self>,
    ) -> EffectAnswer {
        self.perform_effect(effect).map(|()| Effected::done())
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.is_editing() {
            return Vec::new();
        }
        let reason = "Open an item in the editor first";
        vec![
            EffectStatus::unavailable(PasswordsEffect::SAVE, "not_editing", reason),
            EffectStatus::unavailable(PasswordsEffect::CANCEL, "not_editing", reason),
            EffectStatus::unavailable(PasswordsEffect::GENERATE, "not_editing", reason),
        ]
    }

    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        PasswordsSurface::search_entries(self)
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.item_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.item_row(id)
    }
}

#[cfg(test)]
mod tests {
    use accounts::SecretItem;
    use ui::prelude::TestApp;
    use ui::tilo_actions::{self, ConfirmationPolicy, OperationKind};

    use super::*;
    use crate::actions::item_resource_id;
    use ui::tilo_resource::Resource;

    #[test]
    fn every_launch_withholds_the_vault_window_from_captures() {
        const { assert!(<PasswordsShell as ResourceApp>::BEHAVIOUR.capture_protected) };
    }

    /// A private profile whose vault is the test's own: its key in an
    /// in-process secret store, its file in a scratch home.
    struct Profile {
        _home: tempfile::TempDir,
        _paths: tilo_paths::ProfileScope,
        _secrets: ui::secrets::SecretsScope,
    }

    fn profile() -> Profile {
        let home = tempfile::tempdir().unwrap();
        Profile {
            _paths: tilo_paths::scope_profile(home.path()),
            _secrets: ui::secrets::scope_thread(ui::secrets::memory_backend()),
            _home: home,
        }
    }

    /// A clipboard the test reads back.
    #[derive(Default)]
    struct Clipboard(std::sync::Mutex<Option<String>>);

    impl ui::clipboard::ClipboardBackend for Clipboard {
        fn set_text(&self, _target: ui::clipboard::ClipboardTarget, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.into());
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

    fn published(app: &mut TestApp<PasswordsShell>, id: &str) -> Option<tilo_actions::Resource> {
        fn find(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
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

    #[test]
    fn vault_items_are_live_rows_whose_effects_reach_the_vault() {
        let _profile = profile();
        let mut model = crate::PasswordsModel::open();
        let mut item = SecretItem::login("Private bank", "ada@private.example", "not-public-7A");
        item.add_origin("https://private.example").unwrap();
        let id = model.save_item(&mut item).unwrap();
        drop(model);

        let clipboard = std::sync::Arc::new(Clipboard::default());
        let _host = ui::host::enter(
            &ui::host::HostServices::null().with_clipboard(std::sync::Arc::clone(&clipboard) as _),
        );
        let mut app = TestApp::<PasswordsShell>::new().unwrap();
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

        // Copying hands out the secret, so it asks first.
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

        // Filtering and opening are this window's own view: nothing in the
        // document changes and no history is written.
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

        // Deleting asks first, then removes the item from the vault.
        assert!(
            app.effect(&PasswordsEffect::Delete { item: id.0.clone() })
                .is_err(),
            "deleting needs the caller's confirmation"
        );
        assert_eq!(crate::PasswordsModel::open().items().len(), 1);
        app.effect_confirmed(&PasswordsEffect::Delete { item: id.0 })
            .unwrap();
        assert!(crate::PasswordsModel::open().items().is_empty());
        assert!(published(&mut app, &row_id).is_none(), "its row is gone");
    }

    /// Opening an item's anchor, as a search hit or an agent does, shows
    /// it even where a filter hid it; an item the vault lacks is refused.
    #[test]
    fn opening_an_anchor_shows_its_item_and_refuses_one_the_vault_lacks() {
        let _profile = profile();
        let mut model = crate::PasswordsModel::open();
        let mut item = SecretItem::login("Private bank", "ada@private.example", "not-public-7A");
        let id = model.save_item(&mut item).unwrap();
        drop(model);
        let mut app = TestApp::<PasswordsShell>::new().unwrap();
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
            app.surface_mut::<PasswordsSurface>()
                .core()
                .selected
                .as_ref(),
            Some(&id),
            "opening its anchor shows the item, as a search hit does"
        );
    }

    #[test]
    fn an_item_saved_from_the_editor_lands_in_the_vault() {
        let _profile = profile();
        let mut app = TestApp::<PasswordsShell>::new().unwrap();
        app.effect(&PasswordsEffect::Add).unwrap();
        app.harness()
            .click_named("passwords.field.name")
            .expect("the editor shows the name field");
        app.harness().type_text("Mail");
        app.effect(&PasswordsEffect::Generate).unwrap();
        app.effect(&PasswordsEffect::Save).unwrap();
        let items = crate::PasswordsModel::open().items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Mail");
        assert!(items[0].password.len() >= 20, "a generated password");
        assert!(
            app.effect(&PasswordsEffect::Save).is_err(),
            "the editor closed with the save"
        );
    }

    #[test]
    fn captures_show_a_demo_vault_of_their_own() {
        let _profile = profile();
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
            crate::PasswordsModel::open().items().is_empty(),
            "no capture wrote to the person's vault"
        );
    }
}
