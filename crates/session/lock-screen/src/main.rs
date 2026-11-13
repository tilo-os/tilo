//! The lock screen as a process of its own: the session's own lock, shown
//! by an ordinary Tilo app whose window asks the compositor for the session
//! lock instead of being placed by the desktop.
//!
//! What it shows and what it does is [`tilo_sheets::lock`]'s: the same
//! screen, the same effects, the same prompt. What it shows arrives from the
//! session ([`lock_session`]), which publishes the lock as its own resource,
//! and what the person asks goes back to it as actions on that resource, as
//! any app asks anything of its session.

mod lock_session;

use std::cell::RefCell;

use tilo_sheets::lock::{self, LockSeat, Shown};
use ui::application::{
    ApplicationDefinition, ComponentRunnerInputs, NativeProcessHost, PreparedComponentInputs,
};
use ui::launch::{LaunchArgs, LaunchError};

thread_local! {
    /// What the app is lent: the seat the lock screen shows in and takes
    /// the person's password through.
    static SEAT: RefCell<Option<LockSeat>> = const { RefCell::new(None) };
}

fn main() {
    let seat = LockSeat::default();
    seat.show(shown());
    lock_session::serve(&seat);
    SEAT.with(|slot| *slot.borrow_mut() = Some(seat));
    // The standard launch of any app — its record, its capture scenarios and
    // its launch modes — with this process's host in place of the usual one,
    // because the seat is lent per process.
    let spec = ui::framework::launch_spec::<lock::LockApp>().component_host(prepared);
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

/// What the lock screen shows when it opens: the time. Whose session this is, and everything else
/// it says, arrives with the session's first reading.
fn shown() -> Shown {
    Shown {
        clock: ui::clock::time_of_day(&ui::clock::local_now()),
        date: ui::format::day_heading(ui::clock::today(), false),
        ..Shown::default()
    }
}
