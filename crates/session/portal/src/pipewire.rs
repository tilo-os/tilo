//! The producer side of screen share streams: one `PipeWire` node per
//! stream, fed the latest frame the session captured for it.
//!
//! A session implementing [`crate::CaptureStreams`] over the compositor's
//! capture starts a node here for every stream ([`Producer::start`]) and
//! pushes each frame it copied ([`Producer::push`]). Every node has a
//! thread of its own owning its `PipeWire` objects; the frame slot keeps
//! only the latest frame, so a slow consumer never piles up pixels of the
//! screen, and stopping a node ends its thread.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

/// The most nodes at once.
const MAX_STREAMS: usize = 64;

/// One captured frame, RGBA, kept immutable so the realtime callback only
/// copies from it.
#[derive(Clone)]
struct Frame {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

/// What a node is made for.
#[derive(Clone, Debug)]
struct Request {
    app_id: String,
    width: u32,
    height: u32,
    framerate: u32,
}

struct Live {
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Frame>>>,
}

/// The session's screen share nodes.
pub struct Producer {
    streams: Mutex<HashMap<u32, Live>>,
    /// Counts nodes still being set up, so a burst of starts cannot pass
    /// the bound.
    slots: AtomicUsize,
}

impl Producer {
    /// A producer, when the `PipeWire` daemon answers.
    ///
    /// # Errors
    /// The daemon's socket could not be reached.
    pub fn new() -> Result<Self, String> {
        initialise();
        socket()?;
        Ok(Self {
            streams: Mutex::new(HashMap::new()),
            slots: AtomicUsize::new(0),
        })
    }

    /// Start a node for `app_id`'s stream of frames of `width`×`height`;
    /// its id. Blocks up to two seconds while the daemon assigns it.
    ///
    /// # Errors
    /// Too many nodes, or the daemon refused or did not answer.
    pub fn start(
        &self,
        app_id: &str,
        width: u32,
        height: u32,
        framerate: u32,
    ) -> Result<u32, String> {
        if self
            .slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_STREAMS).then(|| count.saturating_add(1))
            })
            .is_err()
        {
            return Err("too many screen share streams".into());
        }
        let free = || {
            self.slots.fetch_sub(1, Ordering::AcqRel);
        };
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let request = Request {
            app_id: app_id.to_owned(),
            width,
            height,
            framerate,
        };
        let (ready, readied) = std::sync::mpsc::sync_channel(1);
        let (frames, stopped) = (Arc::clone(&latest), Arc::clone(&stop));
        if let Err(error) = std::thread::Builder::new()
            .name("portal-screen-stream".into())
            .spawn(move || run_stream(&request, &frames, &stopped, &ready))
        {
            free();
            return Err(format!("no stream thread: {error}"));
        }
        let node = match readied.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(node)) => node,
            Ok(Err(error)) => {
                free();
                return Err(error);
            }
            Err(_) => {
                stop.store(true, Ordering::Release);
                free();
                return Err("the stream did not become ready".into());
            }
        };
        let mut streams = self.streams.lock_recover();
        let reused = streams.contains_key(&node);
        if !reused {
            streams.insert(
                node,
                Live {
                    stop: Arc::clone(&stop),
                    latest,
                },
            );
        }
        drop(streams);
        if reused {
            stop.store(true, Ordering::Release);
            free();
            return Err("PipeWire reused a live node".into());
        }
        Ok(node)
    }

    /// The latest frame of node `node`, `width`×`height` RGBA.
    pub fn push(&self, node: u32, width: u32, height: u32, rgba: Arc<[u8]>) {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if width == 0 || height == 0 || expected != Some(rgba.len()) {
            return;
        }
        let latest = self
            .streams
            .lock_recover()
            .get(&node)
            .map(|live| Arc::clone(&live.latest));
        if let Some(latest) = latest {
            *latest.lock_recover() = Some(Frame {
                width,
                height,
                rgba,
            });
        }
    }

    /// Stop node `node`; stopping a stopped node does nothing.
    pub fn stop(&self, node: u32) {
        let removed = self.streams.lock_recover().remove(&node);
        if let Some(live) = removed {
            live.stop.store(true, Ordering::Release);
            self.slots.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Whether any node streams.
    #[must_use]
    pub fn streaming(&self) -> bool {
        !self.streams.lock_recover().is_empty()
    }
}

fn initialise() {
    static INIT: Once = Once::new();
    INIT.call_once(pipewire::init);
}

fn socket() -> Result<UnixStream, String> {
    let runtime = tilo_paths::runtime_dir()
        .ok_or_else(|| "XDG_RUNTIME_DIR is unset or not absolute".to_owned())?;
    let remote = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| "pipewire-0".into());
    let path = if remote.starts_with('/') {
        std::path::PathBuf::from(remote)
    } else {
        runtime.join(remote)
    };
    UnixStream::connect(&path).map_err(|error| format!("connect {}: {error}", path.display()))
}

/// Copy `frame` into a buffer negotiated at `width`×`height`, scaling by
/// the nearest pixel (no allocation in the realtime callback); the bytes
/// written, whole rows only.
fn copy_scaled(destination: &mut [u8], frame: &Frame, width: u32, height: u32) -> usize {
    let size = |value: u32| usize::try_from(value).ok().filter(|value| *value > 0);
    let (Some(target_width), Some(target_height), Some(source_width), Some(source_height)) = (
        size(width),
        size(height),
        size(frame.width),
        size(frame.height),
    ) else {
        return 0;
    };
    let (Some(target_row), Some(source_row)) =
        (target_width.checked_mul(4), source_width.checked_mul(4))
    else {
        return 0;
    };
    let Some(source) = source_row
        .checked_mul(source_height)
        .and_then(|needed| frame.rgba.get(..needed))
    else {
        return 0;
    };
    let mut written = 0_usize;
    for (y, target) in destination
        .chunks_exact_mut(target_row)
        .take(target_height)
        .enumerate()
    {
        let source_y = scaled(y, source_height, target_height);
        let Some(line) = source.chunks_exact(source_row).nth(source_y) else {
            break;
        };
        if source_row == target_row {
            target.copy_from_slice(line);
        } else {
            for (x, pixel) in target.chunks_exact_mut(4).enumerate() {
                let source_x = scaled(x, source_width, target_width);
                if let Some(from) = line.chunks_exact(4).nth(source_x) {
                    pixel.copy_from_slice(from);
                }
            }
        }
        written = written.saturating_add(target_row);
    }
    written
}

/// Index `index` of `to` items, mapped onto `from` items.
fn scaled(index: usize, from: usize, to: usize) -> usize {
    index.saturating_mul(from).checked_div(to).unwrap_or(0)
}

fn run_stream(
    request: &Request,
    latest: &Arc<Mutex<Option<Frame>>>,
    stop: &AtomicBool,
    ready: &std::sync::mpsc::SyncSender<Result<u32, String>>,
) {
    if let Err(error) = stream_until_stopped(request, latest, stop, ready) {
        let _ = ready.send(Err(error));
    }
}

/// Serve one node until `stop`, telling `ready` its id once the daemon
/// assigned one.
fn stream_until_stopped(
    request: &Request,
    latest: &Arc<Mutex<Option<Frame>>>,
    stop: &AtomicBool,
    ready: &std::sync::mpsc::SyncSender<Result<u32, String>>,
) -> Result<(), String> {
    use pipewire as pw;
    use pw::properties::properties;

    initialise();
    let initial = latest.lock_recover().clone();
    let width = initial
        .as_ref()
        .map_or(request.width, |frame| frame.width)
        .clamp(1, 8192);
    let height = initial
        .as_ref()
        .map_or(request.height, |frame| frame.height)
        .clamp(1, 8192);
    let framerate = request.framerate.clamp(1, 120);
    let failed = |error: pw::Error| error.to_string();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(failed)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(failed)?;
    let core = context.connect_rc(None).map_err(failed)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "Tilo ScreenCast",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_DESCRIPTION => "Tilo screen share",
            *pw::keys::APP_NAME => request.app_id.as_str(),
        },
    )
    .map_err(failed)?;
    let callback_frame = Arc::clone(latest);
    let _listener = stream
        .add_local_listener::<()>()
        .process(move |stream, ()| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let Some(destination) = data.data() else {
                return;
            };
            let frame = callback_frame.lock_recover().clone();
            let bytes = frame.map_or(0, |frame| copy_scaled(destination, &frame, width, height));
            if let Some(rest) = destination.get_mut(bytes..) {
                rest.fill(0);
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = u32::try_from(bytes).unwrap_or(u32::MAX);
            *chunk.stride_mut() = i32::try_from(width.saturating_mul(4)).unwrap_or(i32::MAX);
        })
        .register()
        .map_err(failed)?;
    let values = format(width, height, framerate)?;
    let parameter = pw::spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| "PipeWire rejected the stream format".to_owned())?;
    let mut params = [parameter];
    stream
        .connect(
            pw::spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(failed)?;
    // Node creation is asynchronous. Give the daemon a bounded two seconds
    // to assign the id before the portal reports failure.
    for _ in 0..40 {
        let _ = mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
        let node_id = stream.node_id();
        if node_id != 0 && node_id != u32::MAX {
            if ready.send(Ok(node_id)).is_ok() {
                while !stop.load(Ordering::Acquire) {
                    let _ = mainloop
                        .loop_()
                        .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(16)));
                }
            }
            let _ = stream.disconnect();
            return Ok(());
        }
    }
    let _ = stream.disconnect();
    Err("PipeWire assigned no node".into())
}

/// The stream's one format: RGBA at `width`×`height`, `framerate` per
/// second, serialized.
fn format(width: u32, height: u32, framerate: u32) -> Result<Vec<u8>, String> {
    use pipewire::spa;
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};

    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Id,
            spa::param::video::VideoFormat::RGBA
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            spa::utils::Rectangle { width, height }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            spa::utils::Fraction {
                num: framerate,
                denom: 1
            }
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(values, _)| values.into_inner())
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_scale_into_the_negotiated_buffer_by_the_nearest_pixel() {
        let frame = Frame {
            width: 2,
            height: 2,
            rgba: Arc::from([
                1, 2, 3, 255, // top left
                4, 5, 6, 255, // top right
                7, 8, 9, 255, // bottom left
                10, 11, 12, 255, // bottom right
            ]),
        };
        let mut destination = [0_u8; 4 * 4 * 4];
        let bytes = copy_scaled(&mut destination, &frame, 4, 4);
        assert_eq!(bytes, destination.len());
        assert_eq!(&destination[..4], &[1, 2, 3, 255]);
        assert_eq!(&destination[12..16], &[4, 5, 6, 255]);
        assert_eq!(&destination[48..52], &[7, 8, 9, 255]);
        assert_eq!(&destination[60..64], &[10, 11, 12, 255]);
        let mut short = [0_u8; 8];
        assert_eq!(copy_scaled(&mut short, &frame, 2, 2), 8, "only whole rows");
    }
}
