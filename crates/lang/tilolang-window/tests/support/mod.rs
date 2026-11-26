//! An app's window (Settings' unless a test names another), opened headlessly as a person's device
//! opens it: the app and what it uses compiled from `apps/tl/<app>`, the window laid out at a size, its panes opened through the
//! app's own `open_pane`, and the words it shows read back as assistive technology reads them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "test assertions, and each test binary reads part of this"
)]

use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilolang_tilo::runtime::ResourceRef;
use tilolang_tilo::runtime::{Actor, Value};
use tilolang_window::{Opening, Packages, Window};

pub const APP: &str = "so.tilo.settings";

/// The person the window opens as, as `Opening::new` names them.
pub const PERSON: &str = "ada";

/// The packages the app uses, compiled from `apps/tl/settings` as `tilolang build` compiles them,
/// and the platform's the app reads.
pub fn packages() -> Rc<Packages> {
    packages_of("settings")
}

/// The packages of the app in `apps/tl/<name>` and what it uses, compiled as `tilolang build`
/// compiles them.
pub fn packages_of(name: &str) -> Rc<Packages> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/tl")
        .join(name);
    let search: Vec<PathBuf> = vec![dir.parent().expect("apps/tl").to_path_buf()];
    let (built, platform) = tilolang_tilo::platform::compile(
        &dir,
        &search,
        &mut tilolang_tilo::tilolang::OsEntropy::default(),
    )
    .expect("the app compiles");
    Rc::new(Packages::new(
        Rc::new(built.into_iter().map(|package| package.compiled).collect()),
        platform,
    ))
}

/// The window at `size` in `theme`, laid out and at rest.
pub fn open(size: (u32, u32), theme: tilo_ui_base::Theme) -> Window {
    open_with(&packages(), size, theme)
}

/// [`open`] over packages compiled already: what a test that times opening a window builds first,
/// so the step is the launch and not the app's compilation.
pub fn open_with(packages: &Rc<Packages>, size: (u32, u32), theme: tilo_ui_base::Theme) -> Window {
    open_app(APP, packages, size, theme)
}

/// The window of `app`, one of `packages`, at `size` in `theme`, laid out and at rest.
pub fn open_app(
    app: &str,
    packages: &Rc<Packages>,
    size: (u32, u32),
    theme: tilo_ui_base::Theme,
) -> Window {
    tilo_app_window::i18n::ensure();
    let mut window =
        Window::open(app, packages, Opening::new(size, theme)).expect("the window opens");
    window.exchange();
    window.harness.relayout();
    window.exchange();
    window
}

/// Let the window and its instance answer each other and the layouts that follow, as a live
/// window does after an input.
pub fn settle(window: &mut Window) {
    window.exchange();
    window.harness.relayout();
    window.exchange();
}

/// Let the window draw the frames its motion asks for, up to `bound`, and lay it out again: a
/// pane sliding in from the list or a drawer opening has reached where a person sees it, which a
/// read of the window at the frame the press arrived on would miss.
pub fn settle_motion(window: &mut Window, bound: ui::time::Duration) {
    window.exchange();
    window.harness.settle_for_capture(bound);
    window.harness.relayout();
    window.exchange();
}

/// Every word the window shows, as a screen reader reads them.
pub fn said(window: &Window) -> Vec<String> {
    tilo_ui_base::a11y::texts(window.harness.root_widget())
}

/// Whether any word the window shows holds `words`.
pub fn shows(window: &Window, words: &str) -> bool {
    said(window).iter().any(|text| text.contains(words))
}

/// Every accessible node of the window, as a screen reader reads it.
pub fn accessible(window: &Window) -> Vec<tilo_ui_base::a11y::AccessNode> {
    let snapshot = window.harness.inspection();
    (snapshot.root.nodes().into_iter())
        .filter_map(|node| node.semantics.as_ref())
        .flat_map(tilo_ui_base::a11y::AccessNode::nodes)
        .cloned()
        .collect()
}

/// The app's own action `name`, run with `args` as a search row or an agent runs it, and the
/// frames that show what it changed.
pub fn invoke(window: &mut Window, name: &str, args: Vec<Value>) {
    let host = window.instance.host().expect("the window runs an instance");
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the app publishes no `{name}`"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(PERSON)),
            (id.as_str(), args),
        )
        .unwrap_or_else(|why| panic!("{name}: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(_) => {}
        other => panic!("{name}: {other:?}"),
    }
    settle(window);
}

/// The pane `pane` (`"sound"`, `"power"`) in front, opened as its sidebar row or a search row
/// opens it, and the frames that show it.
pub fn open_pane(window: &mut Window, pane: &str) {
    invoke(window, "open_pane", vec![Value::text(pane)]);
}

/// Set the reading of the device's resource `id`, of the type `<app>#<name>`, to `reading`, as
/// the session publishes it.
pub fn set_reading(window: &Window, id: &str, ty: &str, reading: &str) {
    let target = ResourceRef::parse_id(id, ty.to_owned()).expect("the resource's id");
    window
        .instance
        .host()
        .expect("the window runs an instance")
        .runtime()
        .set_reading(&target, reading)
        .expect("the reading is set");
}
