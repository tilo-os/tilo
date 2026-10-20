//! A read of the device made off the UI thread.
//!
//! An area of Settings whose rows show what it reads of the device — its
//! audio graph, its Bluetooth adapter, its accounts — reads it with blocking
//! calls: commands, D-Bus, file scans. After a window's first frame its
//! warm-up ([`crate::effects::AreaReads`]) starts that read on the host's
//! background executor, and the area builds its model from the result on
//! the UI thread. An area asked for its model while its read still runs —
//! its pane opened, an effect or a row lookup reached it — waits for that
//! read rather than starting a second one, so the device is read once.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::mpsc::{Receiver, TryRecvError};

/// A result on its way from the background executor.
pub type Erased = Box<dyn Any + Send>;

/// One background read of a `T`, started at most once.
///
/// Only its edges know `T`: [`ErasedRead`] carries every result as an
/// [`Erased`] box. Each area reads a type of its own, and a channel per
/// type compiled `std::sync::mpsc` once per area into the Settings tapp,
/// over its size budget.
pub struct DeviceRead<T> {
    read: ErasedRead,
    of: PhantomData<fn() -> T>,
}

impl<T> Default for DeviceRead<T> {
    fn default() -> Self {
        Self {
            read: ErasedRead::default(),
            of: PhantomData,
        }
    }
}

impl<T: Send + 'static> DeviceRead<T> {
    /// Start `read` on the background executor; the window is woken when it
    /// is done. `false` when a read was already started, or when the host
    /// runs no background work — the area then reads when it is asked.
    pub fn start(&self, read: impl FnOnce() -> T + Send + 'static) -> bool {
        self.read
            .start(Box::new(move || Box::new(read()) as Erased))
    }

    /// Whether the read started has ended: its result is in, or it failed
    /// and the area will read when asked. `true` when none was started.
    pub fn is_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// The started read's result, waiting for it if it still runs. `None`
    /// when no read was started or it failed: the caller reads itself.
    pub fn take(&self) -> Option<T> {
        let value = self.read.take()?.downcast::<T>().ok()?;
        Some(*value)
    }
}

/// [`DeviceRead`] for a result of any type: the reading, compiled once.
#[derive(Default)]
struct ErasedRead {
    /// The read on its way, until its result is taken off the channel.
    pending: RefCell<Option<Receiver<Erased>>>,
    /// Its result, once taken off the channel and not yet adopted.
    done: RefCell<Option<Erased>>,
}

impl ErasedRead {
    fn start(&self, read: Box<dyn FnOnce() -> Erased + Send>) -> bool {
        if self.pending.borrow().is_some() || self.done.borrow().is_some() {
            return false;
        }
        let Some(receiver) = spawn_erased(read) else {
            return false;
        };
        *self.pending.borrow_mut() = Some(receiver);
        true
    }

    fn is_finished(&self) -> bool {
        let mut pending = self.pending.borrow_mut();
        let Some(receiver) = pending.as_ref() else {
            return true;
        };
        match receiver.try_recv() {
            Ok(value) => {
                *self.done.borrow_mut() = Some(value);
                *pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                *pending = None;
                true
            }
        }
    }

    fn take(&self) -> Option<Erased> {
        if let Some(value) = self.done.borrow_mut().take() {
            return Some(value);
        }
        let receiver = self.pending.borrow_mut().take()?;
        receiver.recv().ok()
    }
}

/// Mark a call that reaches the device or a service — a command, a D-Bus
/// call, a walk of a large tree — and so may take seconds. In a debug build
/// it panics on the UI thread: a pane that makes the window wait on the
/// device fails its tests and a development build, instead of freezing the
/// window on a slow device. The stand-ins a test reads mark their calls
/// too, so a test that opens a pane on the UI thread proves it reads
/// nothing there.
#[track_caller]
pub fn off_ui_thread() {
    debug_assert!(
        !ui::rt::is_ui_thread(),
        "Settings reached the device on the UI thread; read it off the UI thread \
         (crate::device_read::Reading, DeviceRead or an effect job)"
    );
}

/// What an area last read of the device, read again off the UI thread.
///
/// A pane built over the area shows the last reading, or that the device is
/// still being read, and takes in a new reading at the sync its landing
/// wakes ([`Reading::revision`]). Asking for a read while one runs reads
/// once more after it, so a reading asked for after a change shows it.
pub struct Reading<T> {
    read: ErasedReading,
    of: PhantomData<fn() -> T>,
}

impl<T> Clone for Reading<T> {
    fn clone(&self) -> Self {
        Self {
            read: self.read.clone(),
            of: PhantomData,
        }
    }
}

/// A reading of any type, shared between the panes and the worker.
type Shared = std::sync::Arc<dyn Any + Send + Sync>;

impl<T: Clone + Send + Sync + 'static> Reading<T> {
    /// Nothing read yet; [`Self::refresh`] reads with `read`.
    pub fn new(read: impl Fn() -> T + Send + Sync + 'static) -> Self {
        Self {
            read: ErasedReading::new(std::sync::Arc::new(move || {
                std::sync::Arc::new(read()) as Shared
            })),
            of: PhantomData,
        }
    }

    /// The last reading; `None` until the first lands.
    pub fn get(&self) -> Option<T> {
        self.read.get()?.downcast_ref::<T>().cloned()
    }

    /// Bumped by every reading that lands.
    pub fn revision(&self) -> u64 {
        self.read.value.revision()
    }

    /// Change the last reading in place: what a change the device took
    /// makes of it before the device is read again.
    pub fn update(&self, change: impl FnOnce(&mut T)) {
        if let Some(mut value) = self.get() {
            change(&mut value);
            self.read.value.set(Some(std::sync::Arc::new(value)));
        }
    }

    /// Read the device again on the background executor; the window is
    /// woken when the reading lands. A host that runs no background work
    /// reads here.
    pub fn refresh(&self) {
        self.read.refresh();
    }
}

/// [`Reading`] for a reading of any type: the reading, compiled once.
#[derive(Clone)]
struct ErasedReading {
    value: ui::LocalState<Option<Shared>>,
    read: std::sync::Arc<dyn Fn() -> Shared + Send + Sync>,
    /// 0: idle, 1: a read runs, 2: a read runs and another is asked for.
    running: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl ErasedReading {
    fn new(read: std::sync::Arc<dyn Fn() -> Shared + Send + Sync>) -> Self {
        Self {
            value: ui::LocalState::new(None),
            read,
            running: std::sync::Arc::default(),
        }
    }

    fn get(&self) -> Option<Shared> {
        self.value.with(Clone::clone)
    }

    fn refresh(&self) {
        use std::sync::atomic::Ordering;
        // Asked while a read runs: that read goes round once more.
        if self
            .running
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                0 => Some(1),
                _ => Some(2),
            })
            .is_ok_and(|was| was != 0)
        {
            return;
        }
        let this = self.clone();
        // Woken before each reading becomes visible as well as after the work
        // ends: a frame that reads the new value then always has a wake to
        // answer for it, never a change that reached it unannounced.
        let landing = ui::WakeHandle::new();
        let work = Box::new(move || {
            loop {
                this.running.store(1, Ordering::Release);
                let reading = (this.read)();
                landing.wake();
                this.value.set(Some(reading));
                if this
                    .running
                    .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }
            Box::new(()) as Erased
        });
        if spawn_erased(work).is_none() {
            self.value.set(Some((self.read)()));
            self.running.store(0, Ordering::Release);
        }
    }
}

/// Blocking work for the background executor, its result erased.
pub type Work = Box<dyn FnOnce() -> Erased + Send>;

/// Run `work` on the background executor and wake the window asking when
/// it ends; its result comes on the receiver. `None` when the host runs no
/// background work. Every off-thread read and change of Settings goes
/// through here, so the spawn and the channel are compiled once.
fn spawn_erased(work: Work) -> Option<Receiver<Erased>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let wake = ui::WakeHandle::new();
    let job: Box<dyn FnOnce() + Send> = Box::new(move || {
        let result = work();
        // Announced before it can be received, and again once it can: a
        // frame that takes the result always has a wake to answer for it.
        wake.wake();
        let _ = sender.send(result);
        wake.wake();
    });
    ui::background::Background::current()
        .spawn("settings device work", job)
        .ok()
        .map(|()| receiver)
}

/// Start `job` on a thread of its own named `name`, for work that holds a
/// thread for long (a watch loop, a sign-in waiting on the person) or must
/// not queue behind the background executor. Not generic: `std::thread`'s
/// start-up and its result plumbing are compiled once for every thread
/// Settings starts, rather than once per closure (about a kilobyte each).
///
/// # Errors
///
/// When the system refuses a new thread; `job` is dropped unrun, so a
/// channel it held reports the work gone.
pub fn thread(name: &str, job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(job)
        .map(drop)
}

/// Blocking work running off the UI thread for a model, which takes its
/// result in when it lands ([`Landing::poll`]).
pub struct Landing(Receiver<Erased>);

/// Where a [`Landing`] stands.
pub enum Landed<T> {
    /// Still running.
    Pending,
    /// Its result.
    Done(T),
    /// The work stopped without a result (it panicked).
    Lost,
}

impl Landing {
    /// Run `work` off the UI thread. A host that runs no background work
    /// runs it here, so the work is never lost.
    pub fn start<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Self {
        Self::start_erased(Box::new(move || Box::new(work()) as Erased))
    }

    /// [`Self::start`] for work whose result is already erased.
    pub fn start_erased(work: Work) -> Self {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(work)));
        let taken = std::sync::Arc::clone(&slot);
        let run: Work = Box::new(move || {
            let work = taken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            work.map_or_else(|| Box::new(()) as Erased, |work| work())
        });
        if let Some(receiver) = spawn_erased(run) {
            return Self(receiver);
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let work = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(work) = work {
            let _ = sender.send(work());
        }
        Self(receiver)
    }

    /// Its result if it landed, taken once.
    pub fn poll<T: 'static>(&self) -> Landed<T> {
        match self.poll_erased() {
            Landed::Done(value) => value
                .downcast::<T>()
                .map_or(Landed::Lost, |value| Landed::Done(*value)),
            Landed::Pending => Landed::Pending,
            Landed::Lost => Landed::Lost,
        }
    }

    /// [`Self::poll`], its result left erased.
    pub fn poll_erased(&self) -> Landed<Erased> {
        match self.0.try_recv() {
            Ok(value) => Landed::Done(value),
            Err(TryRecvError::Empty) => Landed::Pending,
            Err(TryRecvError::Disconnected) => Landed::Lost,
        }
    }

    /// Wait for its result: only ever off the UI thread. `None` when the
    /// work stopped without one.
    pub fn wait_erased(&self) -> Option<Erased> {
        off_ui_thread();
        self.0.recv().ok()
    }
}

/// The context an effect of Settings answers in.
pub type Cx = ui::framework::EffectCx<crate::resource::Settings, crate::settings_app::SettingsApp>;

/// What takes a job's erased result in on the UI thread.
type ErasedThen = Box<
    dyn FnOnce(
        &mut crate::settings_app::SettingsApp,
        Erased,
        &mut Cx,
    ) -> ui::framework::EffectAnswer,
>;

/// Run `work` off the UI thread as the effect's job, then `then` with its
/// result on the UI thread: [`ui::framework::EffectCx::thread`], with the
/// result erased so the framework's job plumbing is compiled once for
/// every effect of Settings rather than once per result type.
pub fn job<T: Send + 'static>(
    cx: &mut Cx,
    work: impl FnOnce() -> T + Send + 'static,
    then: impl FnOnce(&mut crate::settings_app::SettingsApp, T, &mut Cx) -> ui::framework::EffectAnswer
    + 'static,
) -> ui::framework::Effected {
    erased_job(
        cx,
        Box::new(move || Box::new(work()) as Erased),
        Box::new(move |app, value, cx| match value.downcast::<T>() {
            Ok(value) => then(app, *value, cx),
            Err(_) => Err("a job finished with a value of another type".into()),
        }),
    )
}

fn erased_job(cx: &mut Cx, work: Work, then: ErasedThen) -> ui::framework::Effected {
    cx.thread(
        move |_| work(),
        move |app: &mut crate::settings_app::SettingsApp, value: Erased, cx| then(app, value, cx),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read runs off the calling thread, its result is taken once, and a
    /// second start while it is held is refused.
    #[test]
    fn a_started_read_runs_off_the_calling_thread_once() {
        let read = DeviceRead::default();
        let caller = std::thread::current().id();
        assert!(read.start(move || std::thread::current().id() != caller));
        assert!(!read.start(|| false), "one read at a time");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !read.is_finished() {
            assert!(std::time::Instant::now() < deadline, "the read finishes");
            std::thread::yield_now();
        }
        assert_eq!(read.take(), Some(true), "the read ran on another thread");
        assert_eq!(read.take(), None, "a result is adopted once");
    }

    /// Asking for the result while the read runs waits for it rather than
    /// reading again.
    #[test]
    fn taking_a_running_read_waits_for_it() {
        let read = DeviceRead::default();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        assert!(read.start(move || {
            let _ = gate.recv();
            7
        }));
        assert!(!read.is_finished(), "the read is held at its gate");
        release.send(()).unwrap();
        assert_eq!(read.take(), Some(7));
        assert!(read.is_finished(), "nothing is left on its way");
    }
}
