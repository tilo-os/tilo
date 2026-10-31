//! The shell on the native session: the tilolang shell package's top bar
//! (`apps/tl/shell`), shown as a layer by a [`ShellPart`], kept in step with
//! the session's desk (`crate::desk`) and asking the session's door for what
//! the person presses.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_compositor::Compositor;
use tilo_surface_host::shell_part::Ask;
use tilo_surface_host::{Doors, SessionLink, ShellPart, ShellWork};

use crate::desk::Desk;
use crate::panels::Held;

/// The shell package's files and its ids lock, as they ship with the session: every `.tl` file
/// of the package is part of its one module.
const FILES: [(&str, &str); 5] = [
    ("app.tl", include_str!("../../../../apps/tl/shell/app.tl")),
    ("chrome.tl", include_str!("../../../../apps/tl/shell/chrome.tl")),
    ("docks.tl", include_str!("../../../../apps/tl/shell/docks.tl")),
    ("layout.tl", include_str!("../../../../apps/tl/shell/layout.tl")),
    ("system.tl", include_str!("../../../../apps/tl/shell/system.tl")),
];
const IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");

/// The shell package compiled, or why it does not compile.
///
/// # Errors
/// The package does not compile: its diagnostics.
pub fn compile() -> Result<tilolang_tilo::tilolang::Compiled, String> {
    let lock = tilolang_tilo::tilolang::IdsLock::parse(IDS).map_err(|error| error.to_string())?;
    let source = tilolang_tilo::tilolang::PackageSource::new(
        FILES
            .into_iter()
            .map(|(path, text)| (format!("apps/tl/shell/{path}"), text.to_owned())),
    );
    tilolang_tilo::tilolang::compile_package_source(&source, &lock, &[]).map_err(|diagnostics| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                diagnostic.render(
                    diagnostic
                        .file
                        .as_deref()
                        .unwrap_or("apps/tl/shell/app.tl"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// The height of the band the chrome leaves the shell's bar, logical pixels.
fn band() -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a bar's height, a few dozen pixels"
    )]
    let band = f64::from(tilo_desktop::topbar::topbar_height(&ui::Theme::dark())).ceil() as u32;
    band
}

/// The shell record's edge settings, as the session reads them for the part's place.
struct EdgeSettings {
    bar_shown: bool,
    /// `top` or `bottom`; anything else (a leading or trailing bar, which the view does not
    /// place today) leaves the band to the desk.
    bar_edge: String,
    dock_shown: bool,
    dock_edge: String,
}

/// The shell's bar on the session, and what feeds it.
pub(crate) struct Shell {
    desk: Desk,
    link: Rc<SessionLink>,
    /// What the desk's reading was last made from.
    seen: Option<crate::desk::DeskKey>,
    part: Rc<RefCell<ShellPart>>,
    /// The band the chrome leaves the bar, logical pixels: a window reaching into it covers a
    /// bar there.
    band: u32,
}

impl Shell {
    /// The shell's bar over every output of `compositor`, published on `doors`.
    pub(crate) fn mount(
        compositor: &mut Compositor,
        doors: &mut Doors,
        desk: Desk,
        scale: f64,
    ) -> Result<Self, String> {
        let compiled = compile()?;
        let app = compiled.meta.app.clone();
        let link = Rc::new(SessionLink::default());
        let wake = compositor.waker();
        let part = ShellPart::new(
            &app,
            vec![compiled],
            &desk.person,
            Rc::clone(&link),
            // The bar's band, at rest: what the chrome leaves it.
            band(),
            scale,
            std::sync::Arc::new(move || wake.wake_by_ref()),
        );
        let part = Rc::new(RefCell::new(part));
        compositor.add_host(Box::new(Held(Rc::clone(&part))));
        doors.add(Rc::clone(&part) as tilo_surface_host::Shared);
        let mut shell = Self {
            desk,
            link,
            seen: None,
            part,
            band: band(),
        };
        shell.publish();
        Ok(shell)
    }

    /// Hand the shell the desk when what it is made from changed, and what the screen says of
    /// its edges either way: a bar or dock that hides until reached for follows the pointer and
    /// the windows, which move without the desk changing. Whether anything was published.
    pub(crate) fn publish(&mut self) -> bool {
        let key = self.desk.key();
        let desk_changed = self.seen.replace(key) != Some(key);
        if desk_changed && !self.link.publish(self.desk.reading()) {
            return false;
        }
        desk_changed
    }

    /// Tell the shell what the screen says of its edges: the pointer at the top or bottom edge,
    /// and a window standing where a bar or a dock there rides. What a bar or dock that shows
    /// only sometimes follows; a screen that says the same as before publishes nothing.
    pub(crate) fn publish_edges(&self, compositor: &Compositor) {
        let Some(outputs) = Self::outputs(compositor) else {
            return;
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "a band's height, a few dozen pixels"
        )]
        let band = f64::from(self.band);
        let windows: Vec<tilo_surface_host::shell_part::WindowRect> = compositor
            .windows()
            .into_iter()
            .map(|(_, (x, y, w, h))| tilo_surface_host::shell_part::WindowRect {
                x: f64::from(x),
                y: f64::from(y),
                width: f64::from(w),
                height: f64::from(h),
            })
            .collect();
        let edges = tilo_surface_host::shell_part::screen_edges(
            outputs,
            band,
            compositor.pointer_location(),
            &windows,
        );
        self.link.publish_edges(edges);
    }

    /// The outputs' bounding box `(x, y, w, h)`, as the part spans them: every output that is
    /// on.
    fn outputs(compositor: &Compositor) -> Option<(i32, i32, u32, u32)> {
        let (x0, y0, x1, y1) = compositor
            .outputs()
            .iter()
            .filter_map(|output| output.rect)
            .map(|(x, y, w, h)| {
                (
                    x,
                    y,
                    x.saturating_add(w),
                    y.saturating_add(h),
                )
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))?;
        let width = u32::try_from(x1.saturating_sub(x0)).ok()?;
        let height = u32::try_from(y1.saturating_sub(y0)).ok()?;
        Some((x0, y0, width.max(1), height.max(1)))
    }

    /// Whether the shell asked something of the door the session has not run yet.
    pub(crate) fn has_asks(&self) -> bool {
        self.link.has_asks()
    }

    /// The edges the shell's chrome stands on, read from the shell's own record: a bar at the
    /// top or the bottom (`bar_shown`, `bar_edge`) and a dock at the bottom or the top. A
    /// leading or trailing dock, or a bar or dock the person turned off, leaves that edge's
    /// band to the desk. Until the record holds them, the first shell: a bar at the top alone.
    pub(crate) fn edges(&self) -> tilo_surface_host::shell_part::HostEdges {
        use tilo_surface_host::shell_part::HostEdges;
        let Some(fields) = self.shell_edges() else {
            return HostEdges {
                bar: true,
                bar_top: true,
                dock: false,
                dock_bottom: false,
            };
        };
        HostEdges {
            bar: fields.bar_shown,
            bar_top: fields.bar_edge == "top",
            dock: fields.dock_shown && (fields.dock_edge == "top" || fields.dock_edge == "bottom"),
            dock_bottom: fields.dock_edge == "bottom",
        }
    }

    /// The shell record's edge settings as it holds them: each an enum's case id, which the
    /// package names in `chrome.tl`. `None` until the record exists (the bar alone).
    fn shell_edges(&self) -> Option<EdgeSettings> {
        let part = self.part.borrow();
        let record = part.shell_record()?;
        let package = part.package()?;
        let meta = &package.meta;
        let ty = meta
            .types
            .iter()
            .find(|ty| ty.name == "Shell")?;
        let read = |name: &str| {
            ty.fields
                .iter()
                .find(|field| field.name == name)
                .and_then(|field| record.get(&field.id).cloned())
        };
        let flag = |name: &str| {
            matches!(
                read(name),
                Some(tilolang_tilo::record::DynValue::Register(serde_json::Value::Bool(true)))
            )
        };
        // A case of an enum is its variant's id in the record; `Edge.top` and friends are the
        // names the settings use, so the case is read by the name its variant's id stands for.
        let case = |name: &str, ty_name: &str| -> String {
            let Some(field) = read(name) else {
                return String::new();
            };
            let tilolang_tilo::record::DynValue::Register(serde_json::Value::String(id)) = field
            else {
                return String::new();
            };
            meta.enums
                .iter()
                .find(|en| en.name == ty_name)
                .and_then(|en| en.variants.iter().find(|v| v.id == id))
                .map_or_else(String::new, |v| v.name.clone())
        };
        Some(EdgeSettings {
            bar_shown: flag("bar_shown"),
            bar_edge: case("bar_edge", "Edge"),
            dock_shown: flag("dock_shown"),
            dock_edge: case("dock_edge", "Edge"),
        })
    }

    /// Move the part to the bands the shell's chrome stands in: a bar at the top or bottom, a
    /// dock at the bottom or top. A change of the person's settings moves it on the next
    /// frame.
    pub(crate) fn set_edges(&self, edges: tilo_surface_host::shell_part::HostEdges) {
        self.part.borrow_mut().set_edges(edges);
    }

    /// Run what the shell asked through `invoke` (the session's door).
    pub(crate) fn serve(
        &self,
        invoke: impl FnMut(&str, &str, &Map<String, Value>) -> Result<(), String>,
    ) {
        let mut invoke = invoke;
        self.link.serve(|ask: &Ask| {
            let arguments: Map<String, Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                .collect();
            invoke(&ask.resource, &ask.action, &arguments)
        });
    }

    /// How much work the bar did since the session started.
    pub(crate) fn work(&self) -> ShellWork {
        self.part.borrow().work()
    }

    /// What the bar refused since the session started, in its words.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.part.borrow().refusals().to_vec()
    }

    /// Where the bar's control named `name` stands, logical `(x, y, w, h)`.
    pub(crate) fn control(&self, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.part.borrow_mut().control_bounds(name)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shell_package_compiles_and_its_ids_lock_holds_every_declaration() {
        let compiled = super::compile().unwrap_or_else(|why| panic!("{why}"));
        let lock = compiled.ids.to_text();
        assert_eq!(
            lock,
            super::IDS,
            "apps/tl/shell/app.ids is not the lock the package compiles to; write:\n{lock}"
        );
    }
}
