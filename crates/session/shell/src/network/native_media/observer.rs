//! One-shot `PipeWire` registry custody. The same observed connection is handed
//! to `GStreamer`; no path reconnect or display-row authority substitutes for it.
use super::{AtomicU64, Ordering};
use media::AudioGraph;
use std::os::fd::OwnedFd;

// Cancellation cannot join a blocking native registry call. Retain its permit
// until the actual one-second observer exits, including detached preparations.
static OBSERVERS: AtomicU64 = AtomicU64::new(0);
struct Permit;
impl Permit {
    fn acquire() -> Result<Self, String> {
        OBSERVERS
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                (count < 8).then_some(count.saturating_add(1))
            })
            .map(|_| Self)
            .map_err(|_| "native PipeWire observer capacity".into())
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        OBSERVERS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct Observed {
    pub cookie: u32,
    /// The devices a person picks from. The session's echo-cancellation
    /// nodes are not among them.
    pub graph: AudioGraph,
    /// The echo-cancelled microphone (`id`, `serial`), when the session runs
    /// one.
    pub echo_cancel: Option<(u32, u32)>,
    pub socket: OwnedFd,
}
impl Observed {
    /// The node a call captures from for source `node`/`serial`: the
    /// echo-cancelled microphone when `node` is the default microphone and
    /// the session runs one (it follows the default microphone), otherwise
    /// the source itself.
    pub fn call_capture_node(&self, node: u32, serial: u32) -> (u32, u32) {
        let default = self
            .graph
            .sources
            .iter()
            .any(|entry| entry.id == node && entry.serial == serial && entry.is_default);
        match self.echo_cancel {
            Some(echo) if default => echo,
            _ => (node, serial),
        }
    }
    pub fn contains(&self, cookie: u32, node: u32, serial: u32, input: bool) -> bool {
        self.cookie == cookie
            && (if input {
                &self.graph.sources
            } else {
                &self.graph.sinks
            })
            .iter()
            .any(|entry| entry.id == node && entry.serial == serial)
    }
}
#[cfg(not(target_os = "linux"))]
pub(super) fn observe() -> Result<Observed, String> {
    Err("native PipeWire observation requires Linux".into())
}
#[cfg(target_os = "linux")]
#[allow(
    clippy::too_many_lines,
    reason = "one blocking PipeWire registry round trip whose callbacks share local state"
)]
pub(super) fn observe() -> Result<Observed, String> {
    use pipewire as pw;
    use pw::types::ObjectType;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        os::fd::FromRawFd,
        rc::Rc,
        time::{Duration, Instant},
    };
    let _permit = Permit::acquire()?;
    let started = Instant::now();
    let end = started
        .checked_add(Duration::from_secs(1))
        .ok_or("PipeWire deadline overflow")?;
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|_| "PipeWire loop unavailable")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|_| "PipeWire context unavailable")?;
    let core = context
        .connect_rc(None)
        .map_err(|_| "PipeWire observation unavailable")?;
    let registry = core
        .get_registry_rc()
        .map_err(|_| "PipeWire registry unavailable")?;
    let cookie = Rc::new(Cell::new(None));
    let failed = Rc::new(Cell::new(false));
    let completed = Rc::new(Cell::new(None));
    let nodes = Rc::new(RefCell::new(
        BTreeMap::<u32, (bool, media::AudioDevice)>::new(),
    ));
    let defaults = Rc::new(RefCell::new((None::<String>, None::<String>)));
    let echo_cancel = Rc::new(Cell::new(None::<(u32, u32)>));
    let metadata = Rc::new(RefCell::new(Vec::<(
        pw::metadata::Metadata,
        pw::metadata::MetadataListener,
    )>::new()));
    let core_listener = core
        .add_listener_local()
        .info({
            let cookie = cookie.clone();
            move |info| {
                cookie.set(Some(info.cookie()));
            }
        })
        .error({
            let failed = failed.clone();
            move |_, _, _, _| {
                failed.set(true);
            }
        })
        .done({
            let completed = completed.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE {
                    completed.set(Some(seq));
                }
            }
        })
        .register();
    let registry_listener = registry
        .add_listener_local()
        .global({
            let nodes = nodes.clone();
            let failed = failed.clone();
            let registry = registry.clone();
            let metadata = metadata.clone();
            let defaults = defaults.clone();
            let echo_cancel = echo_cancel.clone();
            move |global| {
                let Some(props) = global.props.as_ref() else {
                    return;
                };
                if global.type_ == ObjectType::Metadata
                    && props.get("metadata.name") == Some("default")
                {
                    if !metadata.borrow().is_empty() {
                        return;
                    }
                    let Ok(object) = registry.bind::<pw::metadata::Metadata, _>(global) else {
                        failed.set(true);
                        return;
                    };
                    let values = defaults.clone();
                    let listener = object
                        .add_listener_local()
                        .property(move |subject, key, _, value| {
                            if subject != 0 {
                                return 0;
                            }
                            let input = match key {
                                Some("default.audio.source") => true,
                                Some("default.audio.sink") => false,
                                _ => return 0,
                            };
                            let name = value
                                .filter(|value| value.len() <= 1024)
                                .and_then(|value| {
                                    serde_json::from_str::<serde_json::Value>(value).ok()
                                })
                                .and_then(|value| {
                                    value
                                        .get("name")
                                        .and_then(|name| name.as_str())
                                        .filter(|name| name.len() <= 512)
                                        .map(str::to_owned)
                                });
                            let mut values = values.borrow_mut();
                            if input {
                                values.0 = name;
                            } else {
                                values.1 = name;
                            }
                            0
                        })
                        .register();
                    metadata.borrow_mut().push((object, listener));
                    return;
                }
                if global.type_ != ObjectType::Node {
                    return;
                }
                let input = match props.get("media.class") {
                    Some("Audio/Source") => true,
                    Some("Audio/Sink") => false,
                    _ => return,
                };
                let Some(serial) = props
                    .get("object.serial")
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|serial| *serial != 0)
                else {
                    return;
                };
                let Some(name) = props
                    .get("node.name")
                    .filter(|name| !name.is_empty() && name.len() <= 512)
                else {
                    return;
                };
                if name.starts_with(media::audio::ECHO_CANCEL_NODE_PREFIX) {
                    if input && name == media::audio::ECHO_CANCEL_SOURCE_NAME {
                        echo_cancel.set(Some((global.id, serial)));
                    }
                    return;
                }
                let label = props.get("node.description").unwrap_or(name);
                let label: String = label
                    .chars()
                    .filter(|value| !value.is_control())
                    .take(64)
                    .collect();
                let mut nodes = nodes.borrow_mut();
                if nodes.len() >= 256 && !nodes.contains_key(&global.id) {
                    failed.set(true);
                    return;
                }
                nodes.insert(
                    global.id,
                    (
                        input,
                        media::AudioDevice {
                            id: global.id,
                            serial,
                            name: name.into(),
                            description: label,
                            is_default: false,
                            volume: None,
                            muted: None,
                            channels: 0,
                            codec: None,
                        },
                    ),
                );
            }
        })
        .global_remove({
            let nodes = nodes.clone();
            move |id| {
                nodes.borrow_mut().remove(&id);
            }
        })
        .register();
    let sync = |sequence| -> Result<(), String> {
        let pending = core
            .sync(sequence)
            .map_err(|_| "PipeWire synchronization failed")?;
        while completed.get() != Some(pending) {
            if failed.get() || Instant::now() >= end {
                return Err("PipeWire observation deadline".into());
            }
            if mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(10)))
                < 0
            {
                return Err("PipeWire loop failed".into());
            }
        }
        if failed.get() {
            return Err("PipeWire observation failed".into());
        }
        Ok(())
    };
    sync(1)?; // All initial registry entries and requested metadata bindings.
    sync(2)?; // All metadata properties generated by those bindings.
    let cookie = cookie.get().ok_or("PipeWire core identity missing")?;
    let mut graph = AudioGraph::default();
    let selected = defaults.borrow();
    for (input, node) in nodes.borrow().values() {
        let mut node = node.clone();
        node.is_default =
            (if *input { &selected.0 } else { &selected.1 }).as_deref() == Some(node.name.as_str());
        (if *input {
            &mut graph.sources
        } else {
            &mut graph.sinks
        })
        .push(node);
    }
    drop(selected);
    if graph.sources.len() > 128 || graph.sinks.len() > 128 {
        return Err("PipeWire observation capacity".into());
    }
    drop(registry_listener);
    // Listener before proxy: no observer callback or registry reader survives.
    for (object, listener) in metadata.borrow_mut().drain(..) {
        drop(listener);
        drop(object);
    }
    drop(registry);
    sync(3)?; // Drain proxy destruction before the socket is relinquished.
    drop(core_listener);
    // SAFETY: this worker exclusively owns the connected core and has removed
    // every listener. PipeWire steals protocol IO, not the core allocation;
    // normal CoreRc drop below disconnects/frees it. This is the upstream
    // xdg-desktop-portal remote handoff sequence. No second reader remains.
    let raw = unsafe { pw::sys::pw_core_steal_fd(core.as_raw_ptr()) };
    if raw < 0 {
        return Err("PipeWire socket handoff unavailable".into());
    }
    // SAFETY: successful steal_fd returns a new owned descriptor exactly once.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    drop(core);
    Ok(Observed {
        cookie,
        graph,
        echo_cancel: echo_cancel.get(),
        socket,
    })
}
