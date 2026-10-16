//! Volume and mute writes off the compositor thread.
//!
//! A media key must answer in the frame it was pressed, and `wpctl` is a
//! subprocess that waits on `PipeWire` for up to 2.5 s: run on the
//! compositor thread, a wedged or restarting daemon froze cursor, input and
//! painting for that long on every press, and an autorepeating volume key
//! queued one freeze per repeat. The keys now compute their readout from the
//! cached sink state and hand the write to [`AudioWriter`]'s one thread.
//!
//! Writes coalesce while the thread is busy. The level is latest-wins. Mute
//! changes compose rather than replace, so two queued toggles cancel and a
//! toggle after an explicit mute inverts it: the sink always ends where the
//! keys, applied in order, would have left it.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::session_runtime::Delivery;
use crate::shell::media_keys::AudioControl;
use crate::shell::osd::Osd;
use crate::shell::status::StatusBridge;
use tilo_lock::MutexRecover as _;

/// What the writer drives. Production is [`SystemAudio`]; tests substitute a
/// recorder that can block to show the keys never wait on it.
pub(crate) trait AudioBackend: Send + 'static {
    /// Set the default sink to `pct` percent.
    fn set_volume(&mut self, pct: f32) -> Result<(), String>;
    fn set_mute(&mut self, muted: bool) -> Result<(), String>;
    fn toggle_mute(&mut self) -> Result<(), String>;
    /// Flip the default source's mute and report where it ended.
    fn toggle_source_mute(&mut self) -> Result<bool, String>;
    /// The default source's mute flag.
    fn source_muted(&mut self) -> Result<bool, String>;
    /// The default sink as it actually is, to correct the tray after a
    /// write failed.
    fn sink_state(&mut self) -> Result<os_actions::VolumeState, String>;
}

/// `wpctl`, through `os-actions`.
pub(crate) struct SystemAudio;

impl AudioBackend for SystemAudio {
    fn set_volume(&mut self, pct: f32) -> Result<(), String> {
        os_actions::set_volume(pct)
    }

    fn set_mute(&mut self, muted: bool) -> Result<(), String> {
        os_actions::set_mute(muted)
    }

    fn toggle_mute(&mut self) -> Result<(), String> {
        os_actions::toggle_mute()
    }

    fn toggle_source_mute(&mut self) -> Result<bool, String> {
        os_actions::toggle_source_mute()
    }

    fn source_muted(&mut self) -> Result<bool, String> {
        os_actions::get_source_state().map(|state| state.muted)
    }

    fn sink_state(&mut self) -> Result<os_actions::VolumeState, String> {
        os_actions::get_volume_state()
    }
}

/// A queued change to the default sink's mute flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MuteChange {
    Set(bool),
    Toggle,
}

impl MuteChange {
    /// `queued` followed by `next`, as the one change that has the same
    /// effect; `None` when they cancel out.
    pub(crate) const fn then(queued: Option<Self>, next: Self) -> Option<Self> {
        match (queued, next) {
            (_, Self::Set(muted)) => Some(Self::Set(muted)),
            (None, Self::Toggle) => Some(Self::Toggle),
            (Some(Self::Toggle), Self::Toggle) => None,
            (Some(Self::Set(muted)), Self::Toggle) => Some(Self::Set(!muted)),
        }
    }
}

/// A queued mic-mute press: whether the source flips (an even number of
/// presses leaves it), and where its readout goes once the state is known.
struct SourceMute {
    toggle: bool,
    readout: Delivery<Osd>,
}

/// The writes waiting for the thread.
#[derive(Default)]
struct Pending {
    volume_pct: Option<f32>,
    sink_mute: Option<MuteChange>,
    source: Option<SourceMute>,
    closed: bool,
}

impl Pending {
    const fn is_idle(&self) -> bool {
        self.volume_pct.is_none() && self.sink_mute.is_none() && self.source.is_none()
    }
}

struct Shared {
    pending: Mutex<Pending>,
    wake: Condvar,
}

/// The media keys' audio writes, applied in order on one thread.
pub(crate) struct AudioWriter {
    shared: Arc<Shared>,
}

impl AudioWriter {
    /// Drive `wpctl`, correcting `status` when a write fails.
    pub(crate) fn system(status: Option<StatusBridge>) -> Self {
        Self::new(SystemAudio, status)
    }

    pub(crate) fn new(backend: impl AudioBackend, status: Option<StatusBridge>) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-audio-writer".into())
            .spawn(move || {
                let mut backend = backend;
                run(&worker, &mut backend, status.as_ref());
            })
        {
            tracing::warn!(%error, "could not start the audio writer; media keys stay silent");
        }
        Self { shared }
    }

    fn queue(&self, change: impl FnOnce(&mut Pending)) {
        let mut pending = self.shared.pending.lock_recover();
        change(&mut pending);
        drop(pending);
        self.shared.wake.notify_one();
    }
}

impl Drop for AudioWriter {
    fn drop(&mut self) {
        self.queue(|pending| pending.closed = true);
    }
}

impl AudioControl for AudioWriter {
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.queue(|pending| pending.volume_pct = Some(pct));
        Ok(())
    }

    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.queue(|pending| {
            pending.sink_mute = MuteChange::then(pending.sink_mute, MuteChange::Set(muted));
        });
        Ok(())
    }

    fn toggle_mute(&self) -> Result<(), String> {
        self.queue(|pending| {
            pending.sink_mute = MuteChange::then(pending.sink_mute, MuteChange::Toggle);
        });
        Ok(())
    }

    fn toggle_source_mute(&self, readout: &Delivery<Osd>) -> Result<(), String> {
        self.queue(|pending| {
            let toggle = pending.source.as_ref().is_none_or(|queued| !queued.toggle);
            pending.source = Some(SourceMute {
                toggle,
                readout: readout.clone(),
            });
        });
        Ok(())
    }
}

/// One batch the thread took from [`Pending`].
struct Batch {
    volume_pct: Option<f32>,
    sink_mute: Option<MuteChange>,
    source: Option<SourceMute>,
}

fn run(shared: &Shared, backend: &mut impl AudioBackend, status: Option<&StatusBridge>) {
    loop {
        let batch = {
            let pending = shared.pending.lock_recover();
            let mut pending = shared
                .wake
                .wait_while(pending, |pending| pending.is_idle() && !pending.closed)
                .unwrap_or_else(PoisonError::into_inner);
            if pending.is_idle() {
                return;
            }
            Batch {
                volume_pct: pending.volume_pct.take(),
                sink_mute: pending.sink_mute.take(),
                source: pending.source.take(),
            }
        };
        apply(batch, backend, status);
    }
}

fn apply(batch: Batch, backend: &mut impl AudioBackend, status: Option<&StatusBridge>) {
    let volume = batch
        .volume_pct
        .map_or(Ok(()), |pct| backend.set_volume(pct));
    let mute = match batch.sink_mute {
        Some(MuteChange::Set(muted)) => backend.set_mute(muted),
        Some(MuteChange::Toggle) => backend.toggle_mute(),
        None => Ok(()),
    };
    if let Err(error) = volume.and(mute) {
        tracing::warn!(target: "tilo::os_actions", %error, "media key volume write failed");
        // The keys published the state they asked for; put the tray back on
        // the state the sink is actually in.
        if let (Some(status), Ok(actual)) = (status, backend.sink_state()) {
            status.set_volume(actual);
        }
    }
    if let Some(source) = batch.source {
        let muted = if source.toggle {
            backend.toggle_source_mute()
        } else {
            backend.source_muted()
        };
        match muted {
            Ok(muted) => source.readout.deliver(Osd::mic_mute(muted)),
            Err(error) => {
                tracing::warn!(target: "tilo::os_actions", %error, "mic mute failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Records every write; the first write blocks until the test opens
    /// `gate`, after telling it through `entered`.
    struct Recording {
        writes: Arc<Mutex<Vec<String>>>,
        entered: Option<mpsc::Sender<()>>,
        gate: Option<mpsc::Receiver<()>>,
        fail_volume: bool,
        sink: os_actions::VolumeState,
        source_muted: bool,
    }

    impl Recording {
        fn record(&mut self, write: String) {
            self.writes.lock().unwrap().push(write);
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
            }
            if let Some(gate) = self.gate.take() {
                gate.recv().unwrap();
            }
        }
    }

    impl AudioBackend for Recording {
        fn set_volume(&mut self, pct: f32) -> Result<(), String> {
            self.record(format!("volume={pct:.1}"));
            if self.fail_volume {
                return Err("wireplumber restarting".into());
            }
            Ok(())
        }
        fn set_mute(&mut self, muted: bool) -> Result<(), String> {
            self.record(format!("mute={muted}"));
            Ok(())
        }
        fn toggle_mute(&mut self) -> Result<(), String> {
            self.record("toggle_mute".into());
            Ok(())
        }
        fn toggle_source_mute(&mut self) -> Result<bool, String> {
            self.record("toggle_source_mute".into());
            self.source_muted = !self.source_muted;
            Ok(self.source_muted)
        }
        fn source_muted(&mut self) -> Result<bool, String> {
            self.record("source_muted".into());
            Ok(self.source_muted)
        }
        fn sink_state(&mut self) -> Result<os_actions::VolumeState, String> {
            Ok(self.sink)
        }
    }

    struct Harness {
        writes: Arc<Mutex<Vec<String>>>,
        entered: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
    }

    fn blocking_backend(fail_volume: bool) -> (Recording, Harness) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        (
            Recording {
                writes: Arc::clone(&writes),
                entered: Some(entered_tx),
                gate: Some(gate),
                fail_volume,
                sink: os_actions::VolumeState {
                    level: 0.3,
                    muted: true,
                },
                source_muted: false,
            },
            Harness {
                writes,
                entered,
                release,
            },
        )
    }

    fn wait_for(writes: &Mutex<Vec<String>>, count: usize) -> Vec<String> {
        let started = Instant::now();
        loop {
            let seen = writes.lock().unwrap().clone();
            if seen.len() >= count || started.elapsed() > Duration::from_secs(10) {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn mute_changes_compose_in_order() {
        use MuteChange::{Set, Toggle};
        assert_eq!(MuteChange::then(None, Toggle), Some(Toggle));
        assert_eq!(
            MuteChange::then(Some(Toggle), Toggle),
            None,
            "two presses of the mute key leave the sink as it was"
        );
        assert_eq!(MuteChange::then(Some(Set(true)), Toggle), Some(Set(false)));
        assert_eq!(MuteChange::then(Some(Toggle), Set(false)), Some(Set(false)));
    }

    #[test]
    fn volume_keys_coalesce_latest_wins() {
        let (backend, harness) = blocking_backend(false);
        let bridge = StatusBridge::new();
        bridge.set_volume(os_actions::VolumeState {
            level: 0.40,
            muted: false,
        });
        let writer = AudioWriter::new(backend, None);
        let (readout, _mailbox) = crate::session_runtime::mailbox(Arc::new(|| {}));
        let press = |key| {
            crate::shell::media_keys::apply(
                key,
                &crate::shell::media_keys::Context {
                    actions: &crate::shell::quick_settings::actions::UnavailableQsActions,
                    audio: &writer,
                    status: Some(&bridge),
                    transport_osd: &readout,
                    airplane_mode: false,
                    brightness: crate::shell::media_keys::BrightnessKeys::default(),
                },
            )
        };

        let first = press(crate::shell::media_keys::MediaKey::VolumeUp);
        assert!(
            matches!(first, Some(crate::shell::media_keys::Response::Osd(osd)) if osd.caption() == "Volume 45%")
        );
        harness
            .entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the writer took the first step");
        // The sink is wedged on the first write; the key still answers at once.
        for _ in 0..3 {
            assert!(press(crate::shell::media_keys::MediaKey::VolumeUp).is_some());
        }
        let tray = bridge.snapshot().volume.expect("a tray reading").level;
        assert!(
            (tray - 0.60).abs() < 1e-5,
            "the tray follows every press without waiting for the sink: got {tray}"
        );
        harness.release.send(()).unwrap();
        assert_eq!(
            wait_for(&harness.writes, 2),
            ["volume=45.0", "volume=60.0"],
            "steps queued behind a slow write collapse to the latest"
        );
    }

    #[test]
    fn a_failed_write_puts_the_tray_back_on_the_sink() {
        let (backend, harness) = blocking_backend(true);
        let bridge = StatusBridge::new();
        bridge.set_volume(os_actions::VolumeState {
            level: 0.30,
            muted: true,
        });
        let writer = AudioWriter::new(backend, Some(bridge.clone()));
        crate::shell::media_keys::commit_volume(
            &writer,
            Some(&bridge),
            bridge.snapshot().volume,
            0.35,
        )
        .expect("queued");
        assert_eq!(
            bridge.snapshot().volume,
            Some(os_actions::VolumeState {
                level: 0.35,
                muted: false,
            }),
            "the readout is optimistic"
        );
        harness
            .entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the writer took the step");
        harness.release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while bridge.snapshot().volume.is_some_and(|volume| !volume.muted)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            bridge.snapshot().volume,
            Some(os_actions::VolumeState {
                level: 0.3,
                muted: true,
            }),
            "a failed write must not leave an audible state on the tray"
        );
    }

    #[test]
    fn mic_mute_flips_off_thread_and_reports_where_it_ended() {
        let (backend, harness) = blocking_backend(false);
        let writer = AudioWriter::new(backend, None);
        let (readout, mailbox) = crate::session_runtime::mailbox(Arc::new(|| {}));
        writer.toggle_source_mute(&readout).unwrap();
        harness
            .entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the writer took the press");
        writer.toggle_source_mute(&readout).unwrap();
        writer.toggle_source_mute(&readout).unwrap();
        harness.release.send(()).unwrap();
        assert_eq!(
            wait_for(&harness.writes, 2),
            ["toggle_source_mute", "source_muted"],
            "two presses queued behind the first cancel out, and are answered with a read"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut readouts = Vec::new();
        while readouts.len() < 2 && Instant::now() < deadline {
            readouts.extend(mailbox.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            readouts.iter().map(Osd::caption).collect::<Vec<_>>(),
            [Osd::mic_mute(true).caption(), Osd::mic_mute(true).caption()]
        );
    }
}
