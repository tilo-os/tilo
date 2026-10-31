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

/// The shell package's source and its ids lock, as it ships with the session.
const SOURCE: &str = include_str!("../../../../apps/tl/shell/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");

/// The shell package compiled, or why it does not compile.
///
/// # Errors
/// The package does not compile: its diagnostics.
pub fn compile() -> Result<tilolang_tilo::tilolang::Compiled, String> {
    let lock = tilolang_tilo::tilolang::IdsLock::parse(IDS).map_err(|error| error.to_string())?;
    tilolang_tilo::tilolang::compile_package(SOURCE, &lock, &[]).map_err(|diagnostics| {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render("apps/tl/shell/app.tl"))
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

/// The shell's bar on the session, and what feeds it.
pub(crate) struct Shell {
    desk: Desk,
    /// The due timer ids announced already, so an unchanged one is announced once.
    announced: std::cell::RefCell<std::collections::BTreeSet<String>>,
    link: Rc<SessionLink>,
    /// What the desk's reading was last made from.
    seen: Option<crate::desk::DeskKey>,
    part: Rc<RefCell<ShellPart>>,
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
            announced: std::cell::RefCell::new(std::collections::BTreeSet::new()),
            link,
            seen: None,
            part,
        };
        shell.publish();
        Ok(shell)
    }

    /// A timer Clocks says came due that the bar has not announced: its id, and what to say
    /// in the app's own words. Each id is announced once.
    pub(crate) fn new_due_timer(&self) -> Option<(String, String, String)> {
        let (id, title, body) = self.desk.due_timer()?;
        let mut announced = self.announced.borrow_mut();
        announced.insert(id.clone()).then_some((id, title, body))
    }

    /// Hand the shell the desk when what it is made from changed. Whether it did.
    pub(crate) fn publish(&mut self) -> bool {
        let key = self.desk.key();
        if self.seen.replace(key) == Some(key) {
            return false;
        }
        self.link.publish(self.desk.reading())
    }

    /// Whether the shell asked something of the door the session has not run yet.
    pub(crate) fn has_asks(&self) -> bool {
        self.link.has_asks()
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
