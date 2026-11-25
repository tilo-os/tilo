//! A part of the shell whose view is a tilolang package, run by the session itself.
//!
//! A part is a region of the shell the person binds a view to (`settings/shell-views`). The view
//! the session ships with, and most replacements, are programs of their own; a tilolang package
//! is run here instead, in the session's process, by a [`PartView`]: one tilolang instance
//! ([`tilolang_tilo::instance`]) over the package, and the toolkit's widget tree it shows,
//! drawn as a layer like any other widget tree the session shows.
//!
//! What the view reads is the part's own state, published by the session's service as a
//! resource (`desktop:window-menu`) and handed to the instance as the reading of an app the
//! package does not run ([`Runtime::set_reading`]): the session's watch of that resource is the
//! one reading the view ever sees. What the view invokes it invokes on that same resource,
//! through the same action API every other caller uses: the host queues each invocation
//! ([`Asked`]), its arguments in the shapes the verb declares, and the session runs it.
//!
//! The pointer and the keys reach the view while its surface is the one over the desktop, which
//! the session answers ([`PartView::new`]'s `focused`); a press away from what the view drew
//! closes the part, as a press outside a menu does. A package that does not load, or whose view
//! does not come up, is no view for the part: [`PartView::open`] says why, and the session
//! keeps the view it ships with.
//!
//! [`Runtime::set_reading`]: tilolang_runtime::Runtime::set_reading

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_session_core::surface::{CursorIcon, Scroll, SurfaceKey};
use tilo_session_core::surface::{ShellSurface, SurfaceIme, SurfaceLayer, SurfaceTextInput};

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::catalogue;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::wire::resource;
use tilolang_tilo::instance::{Config, Instance, Mode};
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::{Meta, ResourceRef, Value};

use crate::platform::session_platform::{Loads, OnSession};
use crate::platform::surfaces::{Placement, WidgetSurface};

/// How often the instance and its window may answer each other in one frame before the frame
/// is drawn as it is: a press, the invocation it asked for, the answer, the next view run.
const EXCHANGES: usize = 8;

/// The part's state as a view of it written in tilolang reads it: the resource the session
/// publishes for the part, and the surface a package drawing the part is compiled against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartSurface {
    /// The resource's id, as the session publishes it (`desktop:window-menu`).
    pub id: String,
    /// Its kind (`desktop.window-menu`).
    pub kind: String,
    /// The app the surface is read as: the name a package `use`s it by (`desktop`).
    pub app: String,
    /// What the resource is called, as the package titles it.
    pub title: String,
    /// The surface file's text: the fields the reading holds and the verbs it offers
    /// (`tilo_actions::surface`).
    pub text: String,
}

/// The view the session runs for a part: the package, the view in it, and the state it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The app the person bound, as the settings reading names it (`so.tilo.menu`).
    pub app: String,
    /// The view the settings reading names.
    pub view: String,
    /// The package's payload directory.
    pub dir: PathBuf,
    /// The part's state, as the view reads it.
    pub surface: PartSurface,
}

/// One change the view asked the session for: an invocation on the part's resource, of the
/// kind any caller of the action API makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// The resource the view invoked: the part's.
    pub resource: String,
    /// The action, by the verb the session publishes (`move-tab`).
    pub action: String,
    /// Its arguments, by their names. The resource the view acts on is the address's, not an
    /// argument.
    pub arguments: BTreeMap<String, Json>,
    /// The compositor's serial of the person's press the view answered when it asked, when it
    /// answered one: the session runs the invocation as that press ([`PartView::serve`]).
    pub press: Option<u32>,
}

/// The session's API as the view's invoker: what the view asks waits here for the session's
/// next pass, which runs it through the API every other caller uses.
struct PartInvoker {
    /// The app whose surface the view invokes: its own state's.
    meta: Meta,
    /// The part's resource: the one the view acts on.
    target: ResourceRef,
    asked: Rc<RefCell<Vec<Asked>>>,
}

impl PartInvoker {
    /// What the view's invocation passes `action`, in the shape the API takes: its arguments
    /// by name, each as its declared type reads on the API, and not the resource the address
    /// already names.
    fn arguments(
        &self,
        action: &str,
        args: &[(String, Value)],
        names_target: bool,
    ) -> Result<BTreeMap<String, Json>, String> {
        let declared = self
            .meta
            .actions
            .iter()
            .find(|declared| declared.id == action)
            .ok_or_else(|| format!("{} declares no action `{action}`", self.meta.app))?;
        let mut arguments = BTreeMap::new();
        for (name, value) in args.iter().skip(usize::from(names_target)) {
            let param = declared
                .params
                .iter()
                .find(|param| param.name == *name)
                .ok_or_else(|| format!("`{action}` takes no argument `{name}`"))?;
            let ty = tilolang_tilo::runtime::meta::parse_ty(&param.ty, &self.meta.app)?;
            let json = tilolang_tilo::host::argument_json(
                &|other| Err(format!("a part's view names no app but its own: {other}")),
                &self.meta,
                value,
                &ty,
            )?;
            arguments.insert(name.clone(), json);
        }
        Ok(arguments)
    }
}

impl Invoker for PartInvoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action {
            app,
            action,
            name,
            args,
            target,
            ..
        } = invocation
        else {
            return Invoked::Failed(
                "a part's view reads what the session publishes for it; it does not write it"
                    .to_owned(),
            );
        };
        if app.as_str() != self.meta.app || target.as_ref() != Some(&self.target) {
            return Invoked::Failed(format!(
                "a part's view invokes `{name}` on the state it reads, and nothing else"
            ));
        }
        let arguments = match self.arguments(action, args, target.is_some()) {
            Ok(arguments) => arguments,
            Err(why) => return Invoked::Failed(why),
        };
        self.asked.borrow_mut().push(Asked {
            resource: self.target.id_string(),
            // As the session publishes it: the verb, not the id the package names it by.
            action: name.clone(),
            arguments,
            press: tilo_session_core::surface::handled_press(),
        });
        // Taken by the session's next pass: what it made reaches the view as its reading.
        Invoked::Done(Value::Unit)
    }
}

/// What the package's instance asks of the session's process.
struct PartPlatform {
    /// The part's state, as the package knows it: the app the surface is read as.
    meta: Meta,
    /// The package and the modules it uses.
    bundle: Bundle,
    /// Why the package did not load, when it did not.
    why: Rc<RefCell<Option<String>>>,
}

impl Loads for PartPlatform {
    fn load(&self, host: &Host) -> u64 {
        // The platform's packages first, as a shell's own instance loads them: a view of the
        // shell's package reads `so.tilo.session` and `so.tilo.device` beside the part's state.
        // The part's own app is loaded below, from the surface the session publishes for it, and
        // the session's is loaded from the description the view was compiled against.
        if let Ok(packages) = tilolang_tilo::platform_apps::platform_packages() {
            for meta in packages {
                if meta.app == self.meta.app
                    || meta.app == self.bundle.meta.app
                    || meta.app == super::part::SESSION_APP
                {
                    continue;
                }
                let app = meta.app.clone();
                if let Err(error) = host.runtime().load_foreign(meta) {
                    tracing::warn!(%app, %error, "a platform package did not load");
                }
            }
        }
        // The state the view reads exists as an app it does not run, and the package it runs is
        // linked against it: both before either is asked anything.
        if let Err(error) = host.runtime().load_foreign(self.meta.clone()) {
            tracing::warn!(app = self.meta.app, %error, "a part's view did not load");
            *self.why.borrow_mut() = Some(error.to_string());
            return 0;
        }
        match tilolang_tilo::platform_apps::session_package() {
            Ok(session) => {
                if let Err(error) = host.runtime().load_foreign(session) {
                    tracing::warn!(%error, "the session's package did not load");
                }
            }
            Err(error) => tracing::warn!(%error, "the session's description does not read"),
        }
        if let Err(error) = host.load_bundle(&self.bundle) {
            tracing::warn!(app = self.meta.app, %error, "a part's view did not load");
            *self.why.borrow_mut() = Some(error.to_string());
        }
        0
    }
}

/// A part's view, open: its package running, and the tree it draws.
struct Open {
    /// The part it draws, as the settings and the overlay stack name it.
    part: &'static str,
    instance: Instance,
    widgets: WidgetSurface,
    /// What moved since the view last ran in what it keeps of its own, in memory: nothing,
    /// unless its package declares resources of its own.
    moved: tilolang_tilo::resources::Watch,
    /// The part's resource, as the view reads it.
    target: ResourceRef,
    /// What the view asked the session since it last ran it.
    asked: Rc<RefCell<Vec<Asked>>>,
    /// A reading the view has not read yet.
    fresh: bool,
    /// The person's press the view took since it last ran, as the compositor serialed it: what
    /// its next run answers.
    press: Option<u32>,
}

impl Open {
    /// Hand the part's window to `change`, which says whether it changed what the window shows:
    /// only then does the next frame lay the tree out again, so a turn that brought nothing
    /// does no work. Without a window, nothing changed.
    fn change_window(&mut self, change: impl FnOnce(&mut AppWindow) -> bool) -> bool {
        self.widgets.host_mut().change_root(|root| {
            root.as_any_mut()
                .and_then(|any| any.downcast_mut::<AppWindow>())
                .is_some_and(change)
        })
    }

    /// Write what the instance planned to the package's own store, and bring the instance up to
    /// date with it. Whether anything was written.
    fn commit(&mut self) -> bool {
        self.instance.commit_planned();
        if self.moved.take().is_empty() {
            return false;
        }
        self.instance.sync();
        true
    }

    /// What the view said that a part has no window to show.
    fn hear_notices(&mut self) {
        let mut notices = Vec::new();
        self.change_window(|window| {
            notices = window.take_notices();
            false
        });
        for (kind, text) in notices {
            match kind {
                tilo_render_protocol::patch::NoticeKind::Error => {
                    tracing::warn!(
                        text,
                        part = self.part,
                        "a part's view refused what it was asked"
                    );
                }
                tilo_render_protocol::patch::NoticeKind::Info
                | tilo_render_protocol::patch::NoticeKind::Success
                | tilo_render_protocol::patch::NoticeKind::Warning => {
                    tracing::info!(text, part = self.part, "a part's view says");
                }
            }
        }
    }

    /// Let the window and the instance answer each other until neither has more to say, and lay
    /// out what the window took, as the person's press the view took since it last ran.
    fn turn(&mut self) {
        let press = self
            .press
            .take()
            .or_else(tilo_session_core::surface::handled_press);
        tilo_session_core::surface::handling_press(press, || self.turn_once());
    }

    /// The input the view takes now is the person's press the compositor is answering, when it
    /// is one: its next run answers it.
    fn took_press(&mut self) {
        if let Some(press) = tilo_session_core::surface::handled_press() {
            self.press = Some(press);
        }
    }

    fn turn_once(&mut self) {
        let mut shown = false;
        for _ in 0..EXCHANGES {
            self.change_window(AppWindow::flush);
            if std::mem::take(&mut self.fresh) {
                // The reading moved: the view runs again over it.
                self.instance.sync();
            }
            let updated = self.instance.update();
            let committed = self.commit();
            let received = self.change_window(AppWindow::receive);
            self.hear_notices();
            shown |= received;
            if !(updated || committed || received) {
                break;
            }
        }
        if shown {
            self.widgets.relayout();
        }
    }

    /// Draw a frame of the view's tree when it owes one, and say whether it drew or changed
    /// what it shows.
    fn draw(&mut self) -> bool {
        let before = tilo_session_core::surfaces::Published::revision(&self.widgets);
        let drew = self.widgets.prepare_frame();
        drew || tilo_session_core::surfaces::Published::revision(&self.widgets) != before
    }
}

/// One part of the shell, drawn by a tilolang package while the person binds one to it. See
/// the module docs.
pub struct PartView {
    /// The part it draws (`window-menu`), as the overlay stack and the settings name it.
    part: &'static str,
    /// Where the person's displays are, `(x, y, w, h)`, as the compositor last said.
    outputs: Option<(i32, i32, u32, u32)>,
    /// Physical pixels per logical pixel the desktop draws at.
    scale: f64,
    /// Whether this part's surface is the one over the desktop, and so takes the pointer and the
    /// keys: the overlay stack's answer, which the session holds.
    focused: Rc<dyn Fn() -> bool>,
    /// Where the view hangs: the place the reading gives it, in the desktop's coordinates.
    anchor: Option<(f64, f64)>,
    /// The view, while one is open.
    open: Option<Open>,
    /// A press landed away from what the view drew while it held the pointer: the part closes.
    pressed_outside: bool,
    /// Where the pointer was last, in the desktop's coordinates.
    at: Option<(f64, f64)>,
}

impl PartView {
    /// The host of `part`'s tilolang views at `scale`, taking the pointer and the keys while
    /// `focused` says the part's surface is the one over the desktop.
    #[must_use]
    pub fn new(part: &'static str, scale: f64, focused: Rc<dyn Fn() -> bool>) -> Self {
        Self {
            part,
            outputs: None,
            scale,
            focused,
            anchor: None,
            open: None,
            pressed_outside: false,
            at: None,
        }
    }

    /// Run `spec`'s package over `reading` (the part's state, as the session publishes it) for
    /// `person`. `Err` says why the package is no view for the part: it does not read, it
    /// declares no such view, or the view does not run.
    ///
    /// # Errors
    /// As above.
    pub fn open(
        &mut self,
        spec: &Spec,
        person: &str,
        waker: Waker,
        reading: &str,
    ) -> Result<(), String> {
        self.close();
        let bundle = Bundle::read(&spec.dir).map_err(|error| error.to_string())?;
        let Some(view) = bundle
            .meta
            .views
            .iter()
            .find(|declared| declared.name == spec.view)
            .map(|declared| declared.id.clone())
        else {
            return Err(format!("{} declares no view `{}`", spec.app, spec.view));
        };
        let surfaces = catalogue::parse_surfaces(&spec.surface.text)?;
        let meta = catalogue::package_of(&spec.surface.app, &spec.surface.title, &surfaces);
        let ty = format!(
            "{}#{}",
            spec.surface.app,
            catalogue::kind_type_name(&spec.surface.kind)
        );
        let target = ResourceRef::parse_id(&spec.surface.id, ty)
            .ok_or_else(|| format!("`{}` is not a resource", spec.surface.id))?;
        tilo_app_window::i18n::ensure();
        let why = Rc::new(RefCell::new(None));
        let asked = Rc::new(RefCell::new(Vec::new()));
        let (app_end, window_end) = pair();
        let store = tilolang_tilo::resources::ResourceStore::in_memory();
        let mut instance = Instance::new(
            Config {
                app: bundle.meta.app.clone(),
                me: person.into(),
                mode: Mode::Live,
                device: None,
                store: store.clone(),
                sink: None,
                root: tilolang_tilo::host::RootView::default(),
                // The session's shell draws no links of its own: a resource's is minted where
                // its workspace's page is known.
                link_home: None,
            },
            Rc::new(OnSession {
                loads: PartPlatform {
                    meta: meta.clone(),
                    bundle,
                    why: Rc::clone(&why),
                },
                waker,
            }),
            Rc::new(tilolang_tilo::names::Installed),
            Box::new(app_end),
        );
        // A package whose whole view is another app's state has no root of its own to open.
        instance.without_root();
        instance.sync();
        if let Some(reason) = why.borrow().clone() {
            return Err(reason);
        }
        let Some(host) = instance.host() else {
            return Err("the session could not start a runtime for it".to_owned());
        };
        // What the view reads is the part's reading, as the session's watch last had it; what
        // it invokes goes to the session's API, in the shapes the verbs declare.
        host.runtime()
            .set_reading(&target, reading)
            .map_err(|error| error.to_string())?;
        host.runtime().set_invoker(PartInvoker {
            meta,
            target: ResourceRef::clone(&target),
            asked: Rc::clone(&asked),
        });
        instance.open_with(&resource(&target), &view);
        if instance.detail_target().is_none() {
            return Err(format!("its view `{}` did not run", spec.view));
        }
        let window = AppWindow::part(Box::new(window_end));
        let widgets = WidgetSurface::new(
            Box::new(window),
            Placement {
                origin: (0, 0),
                size: (1, 1),
                scale: self.scale,
            },
            false,
        );
        self.anchor = anchor_of(reading);
        self.open = Some(Open {
            part: self.part,
            instance,
            widgets,
            moved: store.watch(),
            target,
            asked,
            fresh: false,
            press: None,
        });
        self.place();
        if let Some(open) = self.open.as_mut() {
            open.turn();
        }
        Ok(())
    }

    /// Stop the view: the part is drawn by what the session ships with again, or draws
    /// nothing.
    pub fn close(&mut self) {
        self.open = None;
        self.anchor = None;
        self.pressed_outside = false;
        self.at = None;
    }

    /// Whether a view is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Hand the view the part's state as the session publishes it now: `reading`, the same JSON
    /// the session's watch of the part's resource carries. Whether the view has a new reading
    /// to run on.
    pub fn publish(&mut self, reading: &str) -> bool {
        let anchor = anchor_of(reading);
        let moved_place = anchor != self.anchor;
        self.anchor = anchor;
        if moved_place {
            self.place();
        }
        let Some(open) = self.open.as_mut() else {
            return false;
        };
        let Some(host) = open.instance.host() else {
            return false;
        };
        match host.runtime().set_reading(&open.target, reading) {
            Ok(moved) => {
                open.fresh |= moved;
                moved
            }
            Err(error) => {
                tracing::warn!(%error, "a part's view did not take its reading");
                false
            }
        }
    }

    /// Whether the view asked the session for something it has not run yet.
    #[must_use]
    pub fn has_asks(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| !open.asked.borrow().is_empty())
    }

    /// Run what the view asked through `invoke` (the session's API), as any caller of the
    /// action protocol does.
    pub fn serve(&self, mut invoke: impl FnMut(&Asked) -> Result<(), String>) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let asked: Vec<Asked> = open.asked.borrow_mut().drain(..).collect();
        for ask in &asked {
            // What the person's press asked runs as that press: an action it calls is their own
            // act, as the same action picked in the palette is.
            let answer = tilo_session_core::surface::handling_press(
                ask.press.or_else(tilo_session_core::surface::handled_press),
                || invoke(ask),
            );
            if let Err(why) = answer {
                tracing::warn!(?ask, why, "the session refused what a part's view asked");
            }
        }
    }

    /// Take whether a press landed away from the view while it held the pointer: the part
    /// closes, as a press outside a menu does.
    pub fn take_pressed_outside(&mut self) -> bool {
        std::mem::take(&mut self.pressed_outside)
    }

    /// What the view says, as assistive technology hears it: the labels of the widgets it
    /// draws.
    pub fn labels(&mut self) -> Vec<String> {
        self.open.as_mut().map_or_else(Vec::new, |open| {
            tilo_session_core::surfaces::Published::accessible_labels(&mut open.widgets)
        })
    }

    /// Where the control assistive technology names `name` stands, in the desktop's logical
    /// layout `(x, y, width, height)`: what a test or a tool presses it by.
    pub fn control_bounds(&mut self, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.open.as_mut()?.widgets.control_bounds(name)
    }

    /// Stand where the reading puts the view: at its anchor, with the space from there to the
    /// edge of the displays. What the view draws in that space is its own.
    fn place(&mut self) {
        let Some((x0, y0, width, height)) = self.outputs else {
            return;
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a place on the desktop, and the space from it in pixels"
        )]
        let space = |at: f64, start: i32, span: u32| -> (i32, u32) {
            let end = start.saturating_add(i32::try_from(span).unwrap_or(i32::MAX));
            let at = (at.round() as i32).clamp(start, end.saturating_sub(1).max(start));
            let space = u32::try_from(end.saturating_sub(at)).unwrap_or(1).max(1);
            (at, space)
        };
        let (ax, ay) = self
            .anchor
            .unwrap_or_else(|| (f64::from(x0), f64::from(y0)));
        let (x, w) = space(ax, x0, width);
        let (y, h) = space(ay, y0, height);
        if let Some(open) = self.open.as_mut() {
            open.widgets.set_placement(Placement {
                origin: (x, y),
                size: (w, h),
                scale: self.scale,
            });
        }
    }

    /// Whether the view holds the pointer and the keys: its surface is the one over the desktop.
    fn focused(&self) -> bool {
        self.open.is_some() && (self.focused)()
    }
}

impl ShellSurface for PartView {
    fn layers(&self) -> Vec<SurfaceLayer> {
        self.open
            .as_ref()
            .map(|open| open.widgets.layers())
            .unwrap_or_default()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        // Window-modal: while the part is over the desktop it takes every press, and one away
        // from what it drew closes it rather than reaching what lies under it.
        self.focused()
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.at = Some((x, y));
        if let Some(open) = self.open.as_mut() {
            open.widgets.pointer_motion(x, y);
        }
    }

    fn pointer_leave(&mut self) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.pointer_leave();
        }
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        if pressed && self.focused() {
            let away = self
                .at
                .zip(self.open.as_ref())
                .is_some_and(|((x, y), open)| !open.widgets.accepts_pointer(x, y));
            self.pressed_outside |= away;
        }
        if let Some(open) = self.open.as_mut() {
            open.took_press();
            open.widgets.pointer_button(button, pressed);
        }
    }

    fn pointer_axis(&mut self, scroll: Scroll) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.pointer_axis(scroll);
        }
    }

    fn pointer_modifiers(&mut self, held: tilo_session_core::surface::KeyModifiers) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.pointer_modifiers(held);
        }
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::SurfaceOutput]) {
        let Some((x0, y0, x1, y1)) = outputs
            .iter()
            .map(|output| {
                let (x, y, w, h) = output.rect;
                (x, y, x.saturating_add(w), y.saturating_add(h))
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (
            u32::try_from(x1.saturating_sub(x0)),
            u32::try_from(y1.saturating_sub(y0)),
        ) else {
            return;
        };
        self.outputs = Some((x0, y0, width, height));
        self.scale = tilo_session_core::surface::SurfaceOutput::scale_at(
            outputs,
            (
                f64::from(x0) + f64::from(width) / 2.0,
                f64::from(y0) + f64::from(height) / 2.0,
            ),
        );
        self.place();
    }

    fn wants_keyboard(&self) -> bool {
        self.focused()
    }

    fn key(&mut self, key: &SurfaceKey) {
        if let Some(open) = self.open.as_mut() {
            open.took_press();
            open.widgets.key(key);
        }
    }

    fn text_input(&self) -> Option<SurfaceTextInput> {
        self.open.as_ref()?.widgets.text_input()
    }

    fn ime(&mut self, event: SurfaceIme) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.ime(event);
        }
    }

    fn cursor(&self) -> CursorIcon {
        self.open
            .as_ref()
            .map_or(CursorIcon::Default, |open| open.widgets.cursor())
    }

    fn prepare_frame(&mut self) -> bool {
        let Some(open) = self.open.as_mut() else {
            return false;
        };
        open.turn();
        open.draw()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.open.as_ref()?.widgets.next_deadline()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.set_frame_interval(interval);
        }
    }
}

/// Where the reading puts the view, in the desktop's coordinates, when it puts it anywhere.
fn anchor_of(reading: &str) -> Option<(f64, f64)> {
    let reading: Json = serde_json::from_str(reading).ok()?;
    let anchor = reading.get("anchor")?;
    Some((anchor.get("x")?.as_f64()?, anchor.get("y")?.as_f64()?))
}
