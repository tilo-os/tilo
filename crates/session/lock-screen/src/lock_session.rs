//! What the lock screen shows, as the session publishes it: the readings the
//! screen draws, and the actions its effects take.
//!
//! The screen is an app of the session like any other: it opens the resource
//! the session publishes for it, draws each reading it publishes, and asks
//! through the same resource. Opening and invoking block, so both run on
//! workers of their own; a reading is drawn by writing it into the local the
//! view draws, which is what wakes the screen.

use std::sync::mpsc::Receiver;

use tilo_sheets::lock::{LockEffect, LockSeat, Shown};
use ui::LocalStateWriter;
use ui::collaboration::SessionResource;
use ui::tilo_resource::Effect as _;

/// The resource the session publishes the lock's state on.
const LOCK: &str = "lock";

/// Follow the session's lock: each reading it publishes is drawn, and what
/// the screen asks is taken as actions on it.
pub fn serve(seat: &LockSeat) {
    let (outgoing, effects) = std::sync::mpsc::channel();
    seat.ask_with(outgoing);
    // The view hands over the local it draws once the screen is built: from
    // then on the worker takes what the screen asks, and writes each reading
    // into that local.
    let effects = std::sync::Mutex::new(Some(effects));
    seat.reaching_session(move |showing| {
        let Some(effects) = effects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let following = std::thread::Builder::new()
            .name("lock-session".into())
            .spawn(move || follow(showing, effects));
        if let Err(error) = following {
            tracing::warn!(%error, "the lock screen follows no session");
        }
    });
}

/// Open the resource, draw each reading it publishes into `showing`, and take
/// what the screen asks for as long as the screen is up: the lock ends with
/// it, so there is nothing to tidy up when the asks end.
fn follow(showing: LocalStateWriter<Shown>, effects: Receiver<LockEffect>) {
    let opened = match SessionResource::open(LOCK) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::warn!(%error, "the lock screen reaches no session");
            return;
        }
    };
    // A reading wakes the worker that draws it, off the screen's own thread:
    // the session publishes while the person waits, and the screen draws what
    // it says as it arrives.
    let watching = opened.clone();
    let drawing = std::thread::Builder::new()
        .name("lock-readings".into())
        .spawn(move || draw_each(&watching, &showing));
    if let Err(error) = drawing {
        tracing::warn!(%error, "the lock screen draws no reading");
        return;
    }
    for effect in effects {
        if let Err(error) = opened.invoke(effect.id(), &effect.arguments()) {
            tracing::warn!(%error, action = effect.id(), "the lock screen asked");
        }
    }
}

/// Draw each reading of `opened` into `showing` as it arrives: the one it
/// opened at, then each one the session publishes after it.
fn draw_each(opened: &SessionResource, showing: &LocalStateWriter<Shown>) {
    let (woke, wakes) = std::sync::mpsc::channel();
    let mut waking = ui::WakeScope::new();
    if let Err(error) = waking.bind(move |_| {
        let _ = woke.send(());
    }) {
        tracing::warn!(%error, "the lock screen hears of no reading");
        return;
    }
    opened.attach(waking.handle());
    loop {
        draw_newest(opened, showing);
        if wakes.recv().is_err() {
            return;
        }
    }
}

/// Draw the newest reading the session published, if the screen has not drawn
/// it: what the session says, with what only the screen can know — whether
/// Caps Lock is on, as its own typing tells it, and whether it draws its touch
/// keyboard — kept as the screen has it.
fn draw_newest(opened: &SessionResource, showing: &LocalStateWriter<Shown>) {
    let mut newest = None;
    while let Some(reading) = opened.latest() {
        // A reading that cannot be read is not one to draw: the next one is
        // drawn instead, rather than the screen showing nothing.
        match reading.map(|reading| Shown::from_reading(&reading)) {
            Ok(Ok(shown)) => newest = Some(shown),
            Ok(Err(error)) => tracing::warn!(%error, "the lock screen's reading"),
            Err(error) => tracing::warn!(%error, "the lock screen's reading"),
        }
    }
    let Some(newest) = newest else {
        return;
    };
    showing.update(|mine| {
        *mine = Shown {
            caps_lock: mine.caps_lock,
            keyboard: mine.keyboard,
            ..newest
        };
    });
}
