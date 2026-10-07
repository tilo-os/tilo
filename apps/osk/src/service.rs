//! The keyboard's connections to the session it types into.
//!
//! The session compositor launches the keyboard on a private Wayland display
//! that alone offers `input-method-v2` and `virtual-keyboard-v1`. Through the
//! input method the compositor says when a text field is focused (and of what
//! kind), and the keyboard composes and commits text into it; the virtual
//! keyboard sends the editing keys (Backspace outside a word, Return).
//! [`OrderedRouter`] keeps the two connections in order.
//!
//! Without an input method (another compositor, `--no-input-method`) the
//! keyboard stays on screen and types everything through the virtual
//! keyboard, or nowhere when that is missing too.
//!
//! Only the keyboard's own process opens these connections, once its binary
//! [serves](serve) the session: a test or a host that mounts the keyboard
//! draws it without typing into anyone's field.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use ui::WakeHandle;

use crate::imclient::{ImState, ImTransition, InputMethodClient, try_input_method};
use crate::inject::{TextInjector, default_injector};
use crate::output::{Channel, OrderedRouter, OskOutput, PreeditEmulator, Step};

static SERVING: AtomicBool = AtomicBool::new(false);

/// Make this process the session's keyboard: its live window takes the
/// compositor's input method and types into the focused field. The binary
/// calls it before running the app.
pub fn serve() {
    SERVING.store(true, Ordering::Relaxed);
}

/// Whether this process serves the session as its keyboard.
pub(crate) fn serving() -> bool {
    SERVING.load(Ordering::Relaxed)
}

/// Whether the keyboard uses the compositor's input method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputMethodChoice {
    /// Use it when the compositor offers it, else stay on screen.
    Auto,
    /// Use it; the keyboard cannot serve without it.
    Require,
    /// Never: an always-visible keyboard.
    Off,
}

/// A keyboard serving the session: the focus changes the input method
/// reports, and where the typed output goes.
pub(crate) struct Connection {
    /// Focus changes; `None` for an always-visible keyboard.
    pub focus: Option<FocusChanges>,
    /// Delivers every output of the keyboard, in order.
    pub output: Box<dyn FnMut(OskOutput)>,
}

/// The focus changes the input method reports, in order.
pub(crate) struct FocusChanges(Option<Receiver<ImTransition>>);

impl FocusChanges {
    /// The changes reported since the last call.
    pub fn take(&mut self) -> Vec<ImTransition> {
        let Some(changes) = &self.0 else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        loop {
            match changes.try_recv() {
                Ok(transition) => taken.push(transition),
                Err(TryRecvError::Empty) => return taken,
                Err(TryRecvError::Disconnected) => {
                    tracing::warn!("osk: the input method connection closed");
                    self.0 = None;
                    return taken;
                }
            }
        }
    }
}

/// Connect the keyboard to the session. `wake` wakes the app when the input
/// method reports a focus change.
///
/// # Errors
/// The input method is required and the compositor does not offer it.
pub(crate) fn connect(choice: InputMethodChoice, wake: WakeHandle) -> Result<Connection, String> {
    let im = match choice {
        InputMethodChoice::Off => None,
        InputMethodChoice::Auto => try_input_method(),
        InputMethodChoice::Require => Some(try_input_method().ok_or_else(|| {
            "--input-method requested but the compositor does not advertise \
             zwp_input_method_manager_v2"
                .to_owned()
        })?),
    };
    let Some(im) = im else {
        let mut injector = default_injector();
        tracing::info!("osk: text-injection backend = {}", injector.backend_name());
        let mut emulator = PreeditEmulator::new();
        return Ok(Connection {
            focus: None,
            output: Box::new(move |output| injector.send(&emulator.symbols(&output))),
        });
    };
    tracing::info!("osk: auto-show via input-method-v2");
    let im = Arc::new(Mutex::new(im));
    let focus = FocusChanges(Some(follow_focus(Arc::clone(&im), wake)));
    let mut editing_keys = default_injector();
    let mut router = OrderedRouter::new();
    Ok(Connection {
        focus: Some(focus),
        output: Box::new(move |output| {
            let Ok(mut im) = im.lock() else {
                return;
            };
            deliver(&mut router, &mut im, editing_keys.as_mut(), output);
        }),
    })
}

/// Read the input method's focus events on a thread of their own, folded
/// through an [`ImState`] into the changes the keyboard follows.
fn follow_focus(im: Arc<Mutex<InputMethodClient>>, wake: WakeHandle) -> Receiver<ImTransition> {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut model = ImState::new();
        loop {
            // Poll without blocking and release the lock at once, so the UI
            // thread can take it to commit typed text. Focus changes are rare
            // and not latency-critical, so a short sleep between polls is
            // fine.
            let events = {
                let Ok(mut im) = im.lock() else {
                    return;
                };
                im.poll()
            };
            let mut changed = false;
            for event in events {
                let transition = model.reduce(event);
                if transition != ImTransition::default() {
                    if send.send(transition).is_err() {
                        return;
                    }
                    changed = true;
                }
            }
            if changed {
                wake.wake();
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
    receive
}

/// Deliver one keyboard output, keeping it ordered after everything
/// delivered before it even when it travels on the other connection.
fn deliver(
    router: &mut OrderedRouter,
    im: &mut InputMethodClient,
    keys: &mut dyn TextInjector,
    output: OskOutput,
) {
    for step in router.route(output) {
        match step {
            Step::Preedit(text) => im.set_preedit(&text),
            Step::Commit(text) => im.commit_text(&text),
            Step::Key(symbol) => keys.send(&[symbol]),
            Step::Barrier(Channel::InputMethod) => im.sync(),
            Step::Barrier(Channel::Keys) => keys.sync(),
        }
    }
}
