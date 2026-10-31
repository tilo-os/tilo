//! The shell on the native session: the tilolang shell package's top bar
//! (`apps/tl/shell`), shown as a layer by a [`ShellPart`], kept in step with
//! the session's desk (`crate::desk`) and asking the session's door for what
//! the person presses.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_compositor::Compositor;
use tilo_surface_host::shell_part::{ArrangementReading, Ask};
use tilo_surface_host::{Doors, SessionLink, ShellPart, ShellWork};

use crate::desk::Desk;
use crate::panels::Held;

/// The shell package and the packages it uses, compiled, the shell last: what the shell's
/// instance runs.
///
/// # Errors
/// A package does not compile: its diagnostics.
pub fn compile() -> Result<Vec<tilolang_tilo::tilolang::Compiled>, String> {
    tilo_surface_host::shell_part::compile_shell()
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
        let packages = compile()?;
        let app = packages
            .last()
            .map(|compiled| compiled.meta.app.clone())
            .ok_or("the shell package did not compile")?;
        let link = Rc::new(SessionLink::default());
        let wake = compositor.waker();
        let part = ShellPart::new(
            &app,
            packages,
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
        };
        shell.publish();
        Ok(shell)
    }

    /// Hand the shell the desk when what it is made from changed. Whether it did.
    pub(crate) fn publish(&mut self) -> bool {
        let key = self.desk.key();
        if self.seen.as_ref() == Some(&key) {
            return false;
        }
        self.seen = Some(key);
        self.link.publish(self.desk.reading())
    }

    /// Hand the shell the person's arrangement, which its views draw, whenever it changed.
    pub(crate) fn publish_arrangement(&self, reading: ArrangementReading) {
        self.link.publish_arrangement(reading);
    }

    /// The link the session publishes the desk on and the shell asks through: what a test
    /// stages a reading on.
    pub(crate) fn link(&self) -> Rc<SessionLink> {
        Rc::clone(&self.link)
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
            // The shell's effects carry their arguments as text, as the outside payload
            // writes them; the door checks each against the action's own fields, so a number
            // or a flag arrives as one.
            let arguments: Map<String, Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), typed(value)))
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

/// `value` as the door's arguments take it: a whole number or a flag as one, else text.
fn typed(value: &str) -> Value {
    if let Ok(number) = value.parse::<i64>() {
        return Value::from(number);
    }
    match value {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        text => Value::String(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shell_package_compiles_and_its_ids_lock_holds_every_declaration() {
        let packages = super::compile().unwrap_or_else(|why| panic!("{why}"));
        let compiled = packages.last().expect("the shell package compiles last");
        let lock = compiled.ids.to_text();
        assert_eq!(
            lock,
            tilo_surface_host::shell_part::shell_ids(),
            "apps/tl/shell/app.ids is not the lock the package compiles to; write:\n{lock}"
        );
    }
}
