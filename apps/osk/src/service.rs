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

use tilo_input_context::wait::{ConnectionWait, Woken};
use ui::WakeHandle;

use crate::imclient::{ImEvent, ImState, ImTransition, InputMethodClient, try_input_method};
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

/// A bounded contextual-action update from the compositor-owned private display.
#[derive(Clone, Debug)]
pub(crate) enum ContextTransition {
    Context {
        generation: u32,
        actions: Vec<tilo_input_context::ContextSlot>,
    },
    Result {
        generation: u32,
        slot: u32,
        succeeded: bool,
        message: String,
    },
}

pub(crate) struct ContextChanges(Option<Receiver<ContextTransition>>);

impl ContextChanges {
    pub fn take(&self) -> Vec<ContextTransition> {
        self.0
            .as_ref()
            .map_or_else(Vec::new, |changes| changes.try_iter().collect())
    }
}

/// A keyboard serving the session: the focus changes the input method
/// reports, and where the typed output goes.
pub(crate) struct Connection {
    /// Focus changes; `None` for an always-visible keyboard.
    pub focus: Option<FocusChanges>,
    pub context: Option<ContextChanges>,
    pub invoke_context: Box<dyn FnMut(u32, u32) -> Result<(), String>>,
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
    let im = match im.map(|im| im.connection_wait().map(|wait| (im, wait))) {
        None => None,
        Some(Ok(followed)) => Some(followed),
        Some(Err(error)) if choice == InputMethodChoice::Require => {
            return Err(format!(
                "cannot wait on the input method connection: {error}"
            ));
        }
        Some(Err(error)) => {
            tracing::warn!("osk: cannot wait on the input method connection: {error}");
            None
        }
    };
    let Some((im, (waiter, nudge))) = im else {
        let mut injector = default_injector();
        tracing::info!("osk: text-injection backend = {}", injector.backend_name());
        let mut emulator = PreeditEmulator::new();
        return Ok(Connection {
            focus: None,
            context: None,
            invoke_context: Box::new(|_, _| {
                Err("contextual input actions require the trusted input method".into())
            }),
            output: Box::new(move |output| injector.send(&emulator.symbols(&output))),
        });
    };
    tracing::info!("osk: auto-show via input-method-v2");
    let im = Arc::new(Mutex::new(im));
    let (context_send, context_receive) = std::sync::mpsc::channel();
    let focus = FocusChanges(Some(follow_focus(
        Arc::clone(&im),
        waiter,
        wake,
        context_send,
    )));
    let invoking = Arc::clone(&im);
    let mut editing_keys = default_injector();
    let mut router = OrderedRouter::new();
    Ok(Connection {
        focus: Some(focus),
        context: Some(ContextChanges(Some(context_receive))),
        invoke_context: Box::new(move |generation, slot| {
            invoking
                .lock()
                .map_err(|_| "input method connection is unavailable".to_owned())?
                .invoke_context(generation, slot)
        }),
        output: Box::new(move |output| {
            let read_socket = {
                let Ok(mut im) = im.lock() else {
                    return;
                };
                deliver(&mut router, &mut im, editing_keys.as_mut(), output)
            };
            if read_socket {
                nudge.nudge();
            }
        }),
    })
}

/// What the focus thread reads from the input method: the live client, or a
/// fake in tests.
trait FocusSource: Send + 'static {
    /// Everything that arrived, without blocking.
    fn poll_events(&mut self) -> (Vec<ImEvent>, Vec<ContextTransition>);
}

impl FocusSource for InputMethodClient {
    fn poll_events(&mut self) -> (Vec<ImEvent>, Vec<ContextTransition>) {
        let events = self.poll();
        (events, self.take_context_changes())
    }
}

/// Read the input method's focus events on a thread of their own, folded
/// through an [`ImState`] into the changes the keyboard follows. The thread
/// sleeps in [`ConnectionWait::wait`] between batches: no input-method event,
/// no wakeup.
fn follow_focus<S: FocusSource>(
    im: Arc<Mutex<S>>,
    waiter: ConnectionWait,
    wake: WakeHandle,
    context_send: std::sync::mpsc::Sender<ContextTransition>,
) -> Receiver<ImTransition> {
    let (send, receive) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("osk-focus".into())
        .spawn(move || {
            let mut model = ImState::new();
            loop {
                // Read without blocking and release the lock at once, so the
                // UI thread can take it to commit typed text.
                let (events, contexts) = {
                    let Ok(mut im) = im.lock() else {
                        return;
                    };
                    im.poll_events()
                };
                let mut changed = false;
                for context in contexts {
                    if context_send.send(context).is_err() {
                        return;
                    }
                    changed = true;
                }
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
                if waiter.wait() == Woken::Closed {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        // The sender went with the closure, so the keyboard sees the input
        // method as closed and stays usable on screen.
        tracing::warn!("osk: cannot follow input-method focus: {error}");
    }
    receive
}

/// Deliver one keyboard output, keeping it ordered after everything
/// delivered before it even when it travels on the other connection.
/// Returns whether it read the input method's socket (a barrier), which
/// leaves events for the focus thread to collect.
fn deliver(
    router: &mut OrderedRouter,
    im: &mut InputMethodClient,
    keys: &mut dyn TextInjector,
    output: OskOutput,
) -> bool {
    let mut read_socket = false;
    for step in router.route(output) {
        match step {
            Step::Preedit(text) => im.set_preedit(&text),
            Step::Commit(text) => im.commit_text(&text),
            Step::Key(symbol) => keys.send(&[symbol]),
            Step::Barrier(Channel::InputMethod) => {
                im.sync();
                read_socket = true;
            }
            Step::Barrier(Channel::Keys) => keys.sync(),
        }
    }
    read_socket
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    /// Stands in for the input-method client: reads what the "compositor"
    /// wrote to the socket and hands out the events queued for it.
    struct FakeSource {
        socket: UnixStream,
        queued: Arc<Mutex<Vec<ImEvent>>>,
        polls: Arc<AtomicUsize>,
        #[cfg(target_os = "linux")]
        thread: Arc<Mutex<Option<libc::pid_t>>>,
    }

    impl FocusSource for FakeSource {
        fn poll_events(&mut self) -> (Vec<ImEvent>, Vec<ContextTransition>) {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `gettid` has no preconditions.
                let tid = unsafe { libc::gettid() };
                *self.thread.lock().expect("thread") = Some(tid);
            }
            self.polls.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 64];
            while matches!((&self.socket).read(&mut buffer), Ok(n) if n > 0) {}
            (
                std::mem::take(&mut *self.queued.lock().expect("queued")),
                Vec::new(),
            )
        }
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_focus_thread_sleeps_until_the_input_method_speaks() {
        let (compositor, client) = UnixStream::pair().expect("socketpair");
        client.set_nonblocking(true).expect("nonblocking");
        let queued = Arc::new(Mutex::new(Vec::new()));
        let polls = Arc::new(AtomicUsize::new(0));
        #[cfg(target_os = "linux")]
        let thread = Arc::new(Mutex::new(None));
        let source = FakeSource {
            socket: client.try_clone().expect("clone"),
            queued: Arc::clone(&queued),
            polls: Arc::clone(&polls),
            #[cfg(target_os = "linux")]
            thread: Arc::clone(&thread),
        };
        let (waiter, nudge) = ConnectionWait::for_socket(OwnedFd::from(client)).expect("wait");
        let (context_send, _contexts) = std::sync::mpsc::channel();
        let changes = follow_focus(
            Arc::new(Mutex::new(source)),
            waiter,
            WakeHandle::new(),
            context_send,
        );
        until("the first read", || polls.load(Ordering::SeqCst) == 1);

        // Phase `no_event_no_wakeup`.
        #[cfg(target_os = "linux")]
        {
            let tid = thread
                .lock()
                .expect("thread")
                .expect("the thread read once");
            tilo_test_env::idle::assert_stays_asleep(&tid.to_string(), "focus thread");
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            polls.load(Ordering::SeqCst),
            1,
            "with no input-method event the focus thread does not read again"
        );

        // Phase `an_event_on_the_socket_is_read_at_once`.
        queued.lock().expect("queued").push(ImEvent::Activate);
        (&compositor).write_all(&[0]).expect("compositor writes");
        let shown = changes
            .recv_timeout(Duration::from_secs(5))
            .expect("the activation is reported");
        assert_eq!(shown.visibility_changed, Some(true));
        until("one read per event", || polls.load(Ordering::SeqCst) == 2);

        // Phase `events_the_ui_thread_read_are_collected_on_its_nudge`.
        queued.lock().expect("queued").push(ImEvent::Deactivate);
        nudge.nudge();
        let hidden = changes
            .recv_timeout(Duration::from_secs(5))
            .expect("the nudge collects the deactivation");
        assert_eq!(hidden.visibility_changed, Some(false));

        // Phase `a_closed_compositor_ends_the_thread_after_its_last_words`.
        queued.lock().expect("queued").push(ImEvent::Activate);
        (&compositor).write_all(&[0]).expect("compositor writes");
        drop(compositor);
        let last = changes
            .recv_timeout(Duration::from_secs(5))
            .expect("what the compositor sent before hanging up is read");
        assert_eq!(last.visibility_changed, Some(true));
        assert!(
            matches!(
                changes.recv_timeout(Duration::from_secs(5)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            ),
            "the focus thread ends when the compositor hangs up"
        );
        drop(nudge);
    }

    #[test]
    fn dropping_the_ui_side_ends_the_focus_thread() {
        let (_compositor, client) = UnixStream::pair().expect("socketpair");
        client.set_nonblocking(true).expect("nonblocking");
        let source = FakeSource {
            socket: client.try_clone().expect("clone"),
            queued: Arc::default(),
            polls: Arc::default(),
            #[cfg(target_os = "linux")]
            thread: Arc::default(),
        };
        let (waiter, nudge) = ConnectionWait::for_socket(OwnedFd::from(client)).expect("wait");
        let (context_send, _contexts) = std::sync::mpsc::channel();
        let changes = follow_focus(
            Arc::new(Mutex::new(source)),
            waiter,
            WakeHandle::new(),
            context_send,
        );
        drop(nudge);
        assert!(matches!(
            changes.recv_timeout(Duration::from_secs(5)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
