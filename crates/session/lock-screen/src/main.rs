//! The lock screen as a process of its own: the session's own lock, shown
//! by an ordinary Tilo app whose window asks the compositor for the session
//! lock instead of being placed by the desktop.
//!
//! What it shows and what it does is [`tilo_sheets::lock`]'s, exactly as the
//! session mounts it in its own process: the same resource, the same
//! effects, the same prompt. Here the app is the process, so the lock is
//! the window's and the compositor gives it every output and every input.

use std::cell::RefCell;

use tilo_sheets::lock::{self, LockSeat, Shown};
use ui::application::{
    ApplicationDefinition, ComponentRunnerInputs, NativeProcessHost, PreparedComponentInputs,
};
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

thread_local! {
    /// What the app is lent: the seat the lock screen shows in and takes
    /// the person's password through.
    static SEAT: RefCell<Option<LockSeat>> = const { RefCell::new(None) };
}

fn main() {
    let seat = LockSeat::default();
    seat.show(shown());
    SEAT.with(|slot| *slot.borrow_mut() = Some(seat));
    let spec = LaunchSpec::from_definition(lock::definition())
        .component_host(prepared)
        .native_services()
        // The lock screen belongs to no workspace and opens before anyone is
        // signed in: it shows what the session told it, and nothing it
        // writes outlives the lock.
        .runs_without_identity()
        .runtime_attachment(|_| false)
        .allowed_options(&["--data-dir"]);
    ui::launch::run_or_exit(&spec);
}

/// The lock screen's app on this process's own host, lent its seat.
fn prepared(
    definition: &ApplicationDefinition,
    _args: &LaunchArgs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let inputs = ComponentRunnerInputs::from_native_process();
    let mut prepared = NativeProcessHost::prepare_window(definition, None, inputs)?;
    if let Some(seat) = SEAT.with(|slot| slot.borrow_mut().take()) {
        prepared.context.lend(seat);
    }
    Ok(prepared)
}

/// What the lock screen shows when it opens: the time, and whose session
/// this is, as the session that started it said.
fn shown() -> Shown {
    Shown {
        clock: ui::clock::time_of_day(&ui::clock::local_now()),
        person: std::env::var("TILO_SURFACE_PERSON").unwrap_or_default(),
        error: None,
        checking: false,
    }
}
