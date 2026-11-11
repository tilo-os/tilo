//! A slot of the shell whose view is a tilolang package, run by the session itself.
//!
//! A slot is a part of the shell the person binds a view to (`settings:shell-views`). The view
//! the session ships with, and most replacements, are programs of their own; a tilolang package
//! is run here instead, in the session's process, by a [`SlotView`]: one tilolang instance
//! ([`tilolang_tilo::instance`]) over the package, and the toolkit's widget tree it shows,
//! drawn as a layer like any other widget tree the session shows.
//!
//! What the view reads is the slot's own state, published by the session's service as a
//! resource (`desktop:window-menu`) and handed to the instance as the reading of an app the
//! package does not run ([`Runtime::set_reading`]): the session's watch of that resource is the
//! one reading the view ever sees. What the view invokes it invokes on that same resource,
//! through the same action door every other caller uses: the host queues each invocation
//! ([`Asked`]), its arguments in the shapes the verb declares, and the session runs it.
//!
//! The pointer and the keys reach the view while its surface is the one over the desk, which
//! the session answers ([`SlotView::new`]'s `focused`); a press away from what the view drew
//! closes the slot, as a press outside a menu does. A package that does not load, or whose view
//! does not come up, is no view for the slot: [`SlotView::open`] says why, and the session
//! keeps the view it ships with.
//!
//! [`Runtime::set_reading`]: tilolang_runtime::Runtime::set_reading

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_session_core::surface::{CursorIcon, HostKey};
use tilo_session_core::surface::{HostIme, HostLayer, HostTextInput, SurfaceHost};

use tilo_render_protocol::patch::local::pair;
use tilo_ui_host::AppWindow;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::catalogue;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::wire::resource;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::{Meta, NameHost, Principal, ResourceRef, Value};

use crate::{Placement, WidgetHost};

/// How often the instance and its window may answer each other in one frame before the frame
/// is drawn as it is: a press, the invocation it asked for, the answer, the next view run.
const EXCHANGES: usize = 8;

/// The slot's state as a view of it written in tilolang reads it: the resource the session
/// publishes for the slot, and the surface a package drawing the slot is compiled against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSurface {
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

/// The view the session runs for a slot: the package, the view in it, and the state it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The app the person bound, as the settings reading names it (`so.tilo.menu`).
    pub app: String,
    /// The view the settings reading names.
    pub view: String,
    /// The package's payload directory.
    pub dir: PathBuf,
    /// The slot's state, as the view reads it.
    pub surface: SlotSurface,
}

/// One change the view asked the session for: an invocation on the slot's resource, of the
/// kind any caller of the action door makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// The resource the view invoked: the slot's.
    pub resource: String,
    /// The action, by the id the session publishes.
    pub action: String,
    /// Its arguments, by their names. The resource the view acts on is the address's, not an
    /// argument.
    pub arguments: BTreeMap<String, Json>,
}

/// The session's door as the view's invoker: what the view asks waits here for the session's
/// next pass, which runs it through the door every other caller uses.
struct SlotInvoker {
    /// The app whose surface the view invokes: its own state's.
    meta: Meta,
    /// The slot's resource: the one the view acts on.
    target: ResourceRef,
    asked: Rc<RefCell<Vec<Asked>>>,
}

impl SlotInvoker {
    /// What the view's invocation passes `action`, in the shape the door takes: its arguments
    /// by name, each as its declared type reads on the door, and not the resource the address
    /// already names.
    fn arguments(
        &self,
        action: &str,
        args: &[(String, Value)],
        subject: bool,
    ) -> Result<BTreeMap<String, Json>, String> {
        let declared = self
            .meta
            .actions
            .iter()
            .find(|declared| declared.id == action)
            .ok_or_else(|| format!("{} declares no action `{action}`", self.meta.app))?;
        let mut arguments = BTreeMap::new();
        for (name, value) in args.iter().skip(usize::from(subject)) {
            let param = declared
                .params
                .iter()
                .find(|param| param.name == *name)
                .ok_or_else(|| format!("`{action}` takes no argument `{name}`"))?;
            let ty = tilolang_tilo::runtime::meta::parse_ty(&param.ty, &self.meta.app)?;
            let json = tilolang_tilo::host::argument_json(
                &|other| Err(format!("a slot's view names no app but its own: {other}")),
                &self.meta,
                value,
                &ty,
            )?;
            arguments.insert(name.clone(), json);
        }
        Ok(arguments)
    }
}

impl Invoker for SlotInvoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action {
            app,
            action,
            name,
            args,
            subject,
            ..
        } = invocation
        else {
            return Invoked::Failed(
                "a slot's view reads what the session publishes for it; it does not write it"
                    .to_owned(),
            );
        };
        if app.as_str() != self.meta.app || subject.as_ref() != Some(&self.target) {
            return Invoked::Failed(format!(
                "a slot's view invokes `{name}` on the state it reads, and nothing else"
            ));
        }
        let arguments = match self.arguments(action, args, subject.is_some()) {
            Ok(arguments) => arguments,
            Err(why) => return Invoked::Failed(why),
        };
        self.asked.borrow_mut().push(Asked {
            resource: self.target.id_string(),
            action: action.clone(),
            arguments,
        });
        // Taken by the session's next pass: what it made reaches the view as its reading.
        Invoked::Done(Value::Unit)
    }
}

/// What the package's instance asks of the session's process.
struct SlotPlatform {
    /// The slot's state, as the package knows it: the app the surface is read as.
    meta: Meta,
    /// The package and the modules it uses.
    bundle: Bundle,
    waker: Waker,
    /// Why the package did not load, when it did not.
    why: Rc<RefCell<Option<String>>>,
}

impl Platform for SlotPlatform {
    fn load(&self, host: &Host) -> u64 {
        // The state the view reads exists as an app it does not run, and the package it runs is
        // linked against it: both before either is asked anything.
        if let Err(error) = host.runtime().load_foreign(self.meta.clone()) {
            tracing::warn!(app = self.meta.app, %error, "a slot's view did not load");
            *self.why.borrow_mut() = Some(error.to_string());
            return 0;
        }
        if let Err(error) = host.load_bundle(&self.bundle) {
            tracing::warn!(app = self.meta.app, %error, "a slot's view did not load");
            *self.why.borrow_mut() = Some(error.to_string());
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_ui_host::i18n::word(id, args, id)
    }

    /// The time of day as the person reads a clock: a slot's view runs in the session, which
    /// holds their settings.
    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
    }

    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        if window {
            tilo_ui_host::i18n::add_catalogues(catalogues);
        } else {
            tilo_ui_host::i18n::add_shared_catalogues(catalogues);
        }
    }

    fn words_revision(&self) -> u64 {
        tilo_ui_host::i18n::revision()
    }

    fn now(&self) -> String {
        ui::clock::local_now()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::clone(&self.waker)
    }
}

/// The person at the device, as the package names them.
struct Person(String);

impl NameHost for Person {
    fn name(&self, who: &Principal) -> String {
        if who.0.is_empty() {
            self.0.clone()
        } else {
            who.0.clone()
        }
    }
}

/// A slot's view, open: its package running, and the tree it draws.
struct Open {
    /// The slot it draws, as the settings and the overlay stack name it.
    slot: &'static str,
    instance: Instance,
    widgets: WidgetHost,
    /// What a view of a slot keeps of its own: nothing, unless its package declares resources
    /// of its own.
    record: tilo_resource::Doc,
    /// The slot's resource, as the view reads it.
    target: ResourceRef,
    /// What the view asked the session since it last ran it.
    asked: Rc<RefCell<Vec<Asked>>>,
    /// A reading the view has not read yet.
    fresh: bool,
}

impl Open {
    fn window(&mut self) -> Option<&mut AppWindow> {
        self.widgets
            .host_mut()
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Commit what the instance planned to the package's own record, as the door would, and
    /// bring the instance up to date with it. Whether anything was committed.
    fn commit(&mut self) -> bool {
        let mut committed = false;
        while let Some((invocation, _)) = self.instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource != HOST_RESOURCE || address.action != COMMIT {
                self.instance.answered(
                    &address,
                    Some("a slot's view runs only its own commits".to_owned()),
                );
                continue;
            }
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            let applied = self
                .instance
                .host()
                .and_then(|host| Some((host.store().take_ticket(ticket)?, host)))
                .ok_or_else(|| format!("no committed action {ticket}"))
                .and_then(|(action, host)| {
                    crate::commit::recorded(&host, &mut self.record, &action)
                });
            if let Err(why) = &applied {
                tracing::error!(why, "a slot view's change could not be committed");
            }
            self.instance.answered(&address, applied.err());
            committed = true;
        }
        if committed {
            // The record holds the changes once they are committed: the instance reads them
            // from its heads.
            self.record.commit();
            self.instance.sync(Some(&self.record));
        }
        committed
    }

    /// What the view said that a slot has no window to show.
    fn hear_notices(&mut self) {
        let Some(window) = self.window() else {
            return;
        };
        for (kind, text) in window.take_notices() {
            match kind {
                tilo_render_protocol::patch::NoticeKind::Error => {
                    tracing::warn!(
                        text,
                        slot = self.slot,
                        "a slot's view refused what it was asked"
                    );
                }
                tilo_render_protocol::patch::NoticeKind::Info => {
                    tracing::info!(text, slot = self.slot, "a slot's view says");
                }
            }
        }
    }

    /// Let the window and the instance answer each other until neither has more to say, and lay
    /// out what the window took.
    fn turn(&mut self) {
        let mut shown = false;
        for _ in 0..EXCHANGES {
            if let Some(window) = self.window() {
                window.flush();
            }
            if std::mem::take(&mut self.fresh) {
                // The reading moved: the view runs again over it.
                self.instance.sync(Some(&self.record));
            }
            let updated = self.instance.update();
            let committed = self.commit();
            let received = self.window().is_some_and(AppWindow::receive);
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
        let before = crate::Published::revision(&self.widgets);
        let drew = self.widgets.prepare_frame();
        drew || crate::Published::revision(&self.widgets) != before
    }
}

/// One slot of the shell, drawn by a tilolang package while the person binds one to it. See
/// the module docs.
pub struct SlotView {
    /// The slot it draws (`window-menu`), as the overlay stack and the settings name it.
    slot: &'static str,
    /// Where the person's displays are, `(x, y, w, h)`, as the compositor last said.
    outputs: Option<(i32, i32, u32, u32)>,
    /// Physical pixels per logical pixel the desktop draws at.
    scale: f64,
    /// Whether this slot's surface is the one over the desk, and so takes the pointer and the
    /// keys: the overlay stack's answer, which the session holds.
    focused: Rc<dyn Fn() -> bool>,
    /// Where the view hangs: the place the reading gives it, in the desk's coordinates.
    anchor: Option<(f64, f64)>,
    /// The view, while one is open.
    open: Option<Open>,
    /// A press landed away from what the view drew while it held the pointer: the slot closes.
    pressed_outside: bool,
    /// Where the pointer was last, in the desk's coordinates.
    at: Option<(f64, f64)>,
}

impl SlotView {
    /// The host of `slot`'s tilolang views at `scale`, taking the pointer and the keys while
    /// `focused` says the slot's surface is the one over the desk.
    #[must_use]
    pub fn new(slot: &'static str, scale: f64, focused: Rc<dyn Fn() -> bool>) -> Self {
        Self {
            slot,
            outputs: None,
            scale,
            focused,
            anchor: None,
            open: None,
            pressed_outside: false,
            at: None,
        }
    }

    /// Run `spec`'s package over `reading` (the slot's state, as the session publishes it) for
    /// `person`. `Err` says why the package is no view for the slot: it does not read, it
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
        tilo_ui_host::i18n::ensure();
        let why = Rc::new(RefCell::new(None));
        let asked = Rc::new(RefCell::new(Vec::new()));
        let (app_end, window_end) = pair();
        let mut instance = Instance::new(
            Config {
                app: bundle.meta.app.clone(),
                me: person.to_owned(),
                mode: Mode::Live,
                device: None,
                index_dir: None,
                record_loader: None,
                sink: None,
                root: tilolang_tilo::host::RootView::default(),
            },
            Rc::new(SlotPlatform {
                meta: meta.clone(),
                bundle,
                waker,
                why: Rc::clone(&why),
            }),
            Rc::new(Person(person.to_owned())),
            Box::new(app_end),
        );
        // A package whose whole view is another app's state has no root of its own to open.
        instance.without_root();
        let record = tilo_resource::Doc::new();
        instance.sync(Some(&record));
        if let Some(reason) = why.borrow().clone() {
            return Err(reason);
        }
        let Some(host) = instance.host() else {
            return Err("the session could not start a runtime for it".to_owned());
        };
        // What the view reads is the slot's reading, as the session's watch last had it; what
        // it invokes goes to the session's door, in the shapes the verbs declare.
        host.runtime()
            .set_reading(&target, reading)
            .map_err(|error| error.to_string())?;
        host.runtime().set_invoker(SlotInvoker {
            meta,
            target: ResourceRef::clone(&target),
            asked: Rc::clone(&asked),
        });
        instance.open_with(&resource(&target), &view);
        if instance.detail_target().is_none() {
            return Err(format!("its view `{}` did not run", spec.view));
        }
        let window = AppWindow::part(Box::new(window_end));
        let widgets = WidgetHost::new(
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
            slot: self.slot,
            instance,
            widgets,
            record,
            target,
            asked,
            fresh: false,
        });
        self.place();
        if let Some(open) = self.open.as_mut() {
            open.turn();
        }
        Ok(())
    }

    /// Stop the view: the slot is drawn by what the session ships with again, or draws
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

    /// Hand the view the slot's state as the session publishes it now: `reading`, the same JSON
    /// the session's watch of the slot's resource carries. Whether the view has a new reading
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
                tracing::warn!(%error, "a slot's view did not take its reading");
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

    /// Run what the view asked through `invoke` (the session's door), as any caller of the
    /// action protocol does.
    pub fn serve(&self, mut invoke: impl FnMut(&Asked) -> Result<(), String>) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let asked: Vec<Asked> = open.asked.borrow_mut().drain(..).collect();
        for ask in &asked {
            if let Err(why) = invoke(ask) {
                tracing::warn!(?ask, why, "the session refused what a slot's view asked");
            }
        }
    }

    /// Take whether a press landed away from the view while it held the pointer: the slot
    /// closes, as a press outside a menu does.
    pub fn take_pressed_outside(&mut self) -> bool {
        std::mem::take(&mut self.pressed_outside)
    }

    /// What the view says, as assistive technology hears it: the labels of the widgets it
    /// draws.
    pub fn labels(&mut self) -> Vec<String> {
        self.open.as_mut().map_or_else(Vec::new, |open| {
            crate::Published::accessible_labels(&mut open.widgets)
        })
    }

    /// Where the control assistive technology names `name` stands, in the desk's logical
    /// layout `(x, y, width, height)`: what a test or a tool presses it by.
    pub fn control_bounds(&mut self, name: &str) -> Option<(f64, f64, f64, f64)> {
        let open = self.open.as_mut()?;
        let (ox, oy) = open.widgets.placement().origin;
        open.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some(name))
            .find_map(|(_, node)| node.bounds())
            .map(|rect| {
                (
                    f64::from(ox) + rect.x0,
                    f64::from(oy) + rect.y0,
                    rect.width(),
                    rect.height(),
                )
            })
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
            reason = "a place on the desk, and the space from it in pixels"
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

    /// Whether the view holds the pointer and the keys: its surface is the one over the desk.
    fn focused(&self) -> bool {
        self.open.is_some() && (self.focused)()
    }
}

impl SurfaceHost for SlotView {
    fn layers(&self) -> Vec<HostLayer> {
        self.open
            .as_ref()
            .map(|open| open.widgets.layers())
            .unwrap_or_default()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        // Window-modal: while the slot is over the desk it takes every press, and one away
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
            open.widgets.pointer_button(button, pressed);
        }
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.pointer_axis(horizontal, vertical);
        }
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::HostedOutput]) {
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
        self.scale = tilo_session_core::surface::HostedOutput::scale_at(
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

    fn key(&mut self, key: &HostKey) {
        if let Some(open) = self.open.as_mut() {
            open.widgets.key(key);
        }
    }

    fn text_input(&self) -> Option<HostTextInput> {
        self.open.as_ref()?.widgets.text_input()
    }

    fn ime(&mut self, event: HostIme) {
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

/// Where the reading puts the view, in the desk's coordinates, when it puts it anywhere.
fn anchor_of(reading: &str) -> Option<(f64, f64)> {
    let reading: Json = serde_json::from_str(reading).ok()?;
    let anchor = reading.get("anchor")?;
    Some((anchor.get("x")?.as_f64()?, anchor.get("y")?.as_f64()?))
}
