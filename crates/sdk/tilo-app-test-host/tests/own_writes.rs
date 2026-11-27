//! What an app writes as itself for the person who invoked one of its effects
//! ([`EffectCx::apply_own`](tilo_app::framework::EffectCx::apply_own)): only what it keeps for
//! itself (`#[action(by_app)]`), only an action of its own resource, only on its own record of the
//! workspace, and only where the invoker may make that change themselves.

use tilo_app::framework::Effected;
use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;
use tilo_resource::Effect;

/// A connector's record of the workspace: which account it is bound to.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ledger {
    pub bound: String,
}

#[resource(
    kind = "test.ledger",
    app = "so.tilo.test-ledger",
    catalogue = "tests/locales"
)]
impl Ledger {
    /// Bind the record to an account.
    #[action(housekeeping, by_app)]
    pub fn bind(&mut self, to: String) {
        self.bound = to;
    }
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.ledger.effects", catalogue = "tests/locales")]
pub enum LedgerEffect {
    /// Connect an account.
    #[effect(kind = "undoable")]
    Connect { to: String },
}

/// The view every app below shows: it answers `connect` by binding the record as the app.
fn view(cx: &ViewCx<Ledger>) -> Element {
    cx.effects(|effect: LedgerEffect, cx| match effect {
        LedgerEffect::Connect { to } => {
            cx.apply_own(Ledger::BIND.with(to))?;
            Ok(Effected::done())
        }
    });
    Box::new(cx.text(|ledger| ledger.bound.clone()))
}

/// The app that declares the ledger, keeping one record of it for the workspace.
struct LedgerApp;

impl ResourceApp for LedgerApp {
    type Resource = Ledger;
    const ID: &'static str = "so.tilo.test-ledger";
    const TITLE: &'static str = "Ledger";

    fn view(cx: &mut ViewCx<Ledger>) -> Element {
        view(cx)
    }
}

/// The same app showing a record of a window's own, not its record of the workspace.
struct WindowLedgerApp;

impl ResourceApp for WindowLedgerApp {
    type Resource = Ledger;
    const ID: &'static str = "so.tilo.test-ledger";
    const TITLE: &'static str = "Ledger";
    const WINDOW_RESOURCE: bool = true;

    fn view(cx: &mut ViewCx<Ledger>) -> Element {
        view(cx)
    }
}

/// Another app, keeping a ledger as its record of the workspace: the ledger's actions are the
/// declaring app's, not its own.
struct OtherApp;

impl ResourceApp for OtherApp {
    type Resource = Ledger;
    const ID: &'static str = "so.tilo.test-other-ledger";
    const TITLE: &'static str = "Other";

    fn view(cx: &mut ViewCx<Ledger>) -> Element {
        view(cx)
    }
}

fn connect(to: &str) -> LedgerEffect {
    LedgerEffect::Connect { to: to.into() }
}

#[test]
fn an_effect_binds_the_apps_own_record_as_the_app() {
    let mut app = TestApp::<LedgerApp>::new().unwrap();
    app.effect(&connect("ada@example")).unwrap();
    app.settle();
    assert_eq!(app.state().bound, "ada@example");
    let written = app.versions().pop().unwrap().context.unwrap();
    assert_eq!(
        written.invoker.unwrap().acting.by,
        tilo_actions::Actor::App("so.tilo.test-ledger".into()),
        "the write is the app's own, not the person's"
    );
    assert!(!app.undo(), "and no step of the person's to take back");
    // A caller reaches it only through the effect: the action itself is offered to nobody.
    assert!(
        app.call("bind", serde_json::json!({"to": "eve@example"}))
            .is_err()
    );
    assert_eq!(app.state().bound, "ada@example");
}

#[test]
fn a_record_that_is_not_the_apps_record_of_the_workspace_is_refused() {
    let mut app = TestApp::<WindowLedgerApp>::new().unwrap();
    let refused = app.effect(&connect("ada@example")).unwrap_err();
    assert!(
        format!("{refused:?}").contains("own record of the workspace"),
        "{refused:?}"
    );
    app.settle();
    assert!(app.state().bound.is_empty());
}

#[test]
fn another_apps_action_is_refused() {
    let mut app = TestApp::<OtherApp>::new().unwrap();
    let refused = app.effect(&connect("ada@example")).unwrap_err();
    assert!(
        format!("{refused:?}").contains("only its own actions"),
        "{refused:?}"
    );
    app.settle();
    assert!(app.state().bound.is_empty());
}

#[test]
fn a_member_who_may_not_make_the_change_has_the_app_make_nothing() {
    let mut app = TestApp::<LedgerApp>::with_role(tilo_permissions::Role::Viewer).unwrap();
    assert!(app.effect(&connect("ada@example")).is_err());
    app.settle();
    assert!(app.state().bound.is_empty());
}
