//! Where a speaker group's sound comes from on the playing device.
//!
//! Playing to a group must take the sound off this device's own speakers:
//! otherwise the local output plays it early and unsynchronized while the
//! group plays it one group latency later. A [`GroupTap`] therefore creates a
//! private null sink, moves the chosen audio into it and exposes its monitor
//! for capture. Every route of the session (the members over the fabric and
//! this device's own output, if it is a member) captures that one monitor and
//! plays through the timed playout path. Dropping the last handle moves the
//! audio back and removes the sink.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

const SINK_NODE_NAME: &str = "tilo-speaker-group";
const SINK_WAIT: Duration = Duration::from_millis(1500);
const SINK_POLL: Duration = Duration::from_millis(50);

/// Which audio a tap moves into the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TapSelection {
    /// Everything this device plays, including streams started later.
    System,
    /// One application stream, by `object.serial`.
    Application(u32),
}

/// A live null sink carrying the group's audio.
pub struct GroupTap {
    selection: TapSelection,
    /// `object.serial` of the null sink, whose monitor routes capture.
    serial: u32,
    /// Holding the client keeps the non-lingering sink alive.
    client: Mutex<Option<(Child, ChildStdin)>>,
    moved_streams: Vec<u32>,
    previous_default: Option<u32>,
}

impl GroupTap {
    #[must_use]
    pub fn serial(&self) -> u32 {
        self.serial
    }

    fn open(selection: TapSelection) -> Result<Self, String> {
        let mut child = Command::new("pw-cli")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start the speaker group sink: {error}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "the speaker group sink has no control input".to_string())?;
        let command = format!(
            "create-node adapter {{ factory.name=support.null-audio-sink node.name={SINK_NODE_NAME} \
             node.description=\"Speaker group\" media.class=Audio/Sink audio.position=[ FL FR ] \
             object.linger=false }}\n"
        );
        if let Err(error) = stdin
            .write_all(command.as_bytes())
            .and_then(|()| stdin.flush())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("create the speaker group sink: {error}"));
        }
        let deadline = Instant::now()
            .checked_add(SINK_WAIT)
            .unwrap_or_else(Instant::now);
        let sink = loop {
            let graph = media::audio::enumerate();
            if let Some(sink) = graph.sinks.iter().find(|sink| sink.name == SINK_NODE_NAME) {
                break (sink.id, sink.serial, graph);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the speaker group sink did not appear".into());
            }
            std::thread::sleep(SINK_POLL);
        };
        let (sink_id, serial, graph) = sink;
        // From here the tap owns the sink, so an early return restores the
        // graph and removes the sink through `Drop`.
        let mut tap = Self {
            selection,
            serial,
            client: Mutex::new(Some((child, stdin))),
            moved_streams: Vec::new(),
            previous_default: None,
        };
        let control = media::audio::real_control();
        match selection {
            TapSelection::System => {
                tap.previous_default = graph
                    .default_sink()
                    .filter(|sink| sink.name != SINK_NODE_NAME)
                    .map(|sink| sink.id);
                control.set_default_sink(sink_id)?;
                for stream in graph
                    .playback_streams
                    .iter()
                    .filter(|stream| !stream.app.starts_with("tilo-"))
                {
                    if control.route_stream(stream.id, serial).is_ok() {
                        tap.moved_streams.push(stream.id);
                    }
                }
            }
            TapSelection::Application(stream_serial) => {
                let stream = graph
                    .playback_streams
                    .iter()
                    .find(|stream| stream.serial == stream_serial)
                    .ok_or_else(|| crate::shell::i18n::t("speaker-error-app-stopped"))?;
                control.route_stream(stream.id, serial)?;
                tap.moved_streams.push(stream.id);
            }
        }
        Ok(tap)
    }
}

impl Drop for GroupTap {
    fn drop(&mut self) {
        let control = media::audio::real_control();
        if let Some(previous) = self.previous_default {
            let _ = control.set_default_sink(previous);
        }
        for stream in &self.moved_streams {
            let _ = control.unroute_stream(*stream);
        }
        if let Ok(mut client) = self.client.lock()
            && let Some((mut child, stdin)) = client.take()
        {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn taps() -> &'static Mutex<Vec<Weak<GroupTap>>> {
    static TAPS: OnceLock<Mutex<Vec<Weak<GroupTap>>>> = OnceLock::new();
    TAPS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Share the live tap for `selection`, or open one. Only one tap exists at a
/// time: a second selection replaces nothing and is refused, because two
/// sinks would fight over the default output.
///
/// # Errors
///
/// A message when the sink cannot be created, the selection's stream is gone,
/// or another group session already owns this device's audio.
pub fn acquire(selection: TapSelection) -> Result<Arc<GroupTap>, String> {
    let mut taps = taps()
        .lock()
        .map_err(|_| crate::shell::i18n::t("speaker-error-audio-unavailable"))?;
    taps.retain(|tap| tap.strong_count() > 0);
    if let Some(live) = taps.iter().find_map(Weak::upgrade) {
        return if live.selection == selection {
            Ok(live)
        } else {
            Err(crate::shell::i18n::t("speaker-error-other-session"))
        };
    }
    let tap = Arc::new(GroupTap::open(selection)?);
    taps.push(Arc::downgrade(&tap));
    drop(taps);
    Ok(tap)
}
