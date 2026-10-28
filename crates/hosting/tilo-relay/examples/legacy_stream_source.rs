//! A native session sharing one legacy window with a host, for the browser
//! streaming qualification (`scripts/perf/native_stream_qualification.mjs`).
//!
//! It is the session half of a legacy window on the browser desktop
//! (`LEGACY_WINDOWS.md`): a device of the dev host's user, certified by its
//! root, subscribes to a room, grants the host `workspace-control:<surface>`
//! and streams the window as `SurfaceStreamStart` and `SurfaceFrame`
//! presence, exactly as the shell's share session does. The pictures are
//! encoded by the production encoder (`tilo_media::RealtimeEncoder`, with
//! the shell's legacy-share configuration: screen content, 8 Mbit/s, a
//! keyframe a second, 60 fps on a hardware encoder and 30 on a software one),
//! so the browser decodes the bitstream it would receive from a real
//! session. Only the Wayland capture is replaced: each picture is drawn
//! here, a text-editor-like window whose caret moves every frame, with the
//! frame's number in a row of black and white blocks ([`draw_code`]), so the
//! page can tell exactly which frame it shows and when it was captured.
//!
//! ```text
//! legacy_stream_source --dev-state DIR --host IROH_ADDR_JSON --room UUID --out FRAMES.jsonl
//!     [--width 1280] [--height 720] [--seconds 10] [--fps N] [--surface 7] [--codec h264|vp9|av1]
//!     [--resize-at S --resize-to WxH]
//! ```
//!
//! `--codec` picks the stream's codec (H.264 by default). VP9 and AV1 are what
//! a session without an H.264 encoder (no hardware one, no codec pack) streams
//! once every browser member decodes them.
//!
//! With `--resize-at`, the window changes to `--resize-to` that many
//! seconds into the stream, as a person resizing the native window does: the
//! session announces the new size (`SurfaceStreamStart`) and encodes from a
//! keyframe at it.
//!
//! `--dev-state` is `tilo-relay dev --state`: its `dev-identity.bin` holds
//! the user root and the host device's keys. Protocol on stdio, one JSON
//! object per stdout line:
//!
//! - `{"event":"ready", …}` once subscribed, with the encoder chosen;
//! - a `start` line on stdin starts the stream: `{"event":"started"}`, then
//!   one line per frame to `--out` (`index`, `captureUs`, `encodeUs`, `bytes`,
//!   `key`, `width`, `height`, wall-clock microseconds since the Unix epoch);
//! - `{"event":"input", …}` for every input event the host forwards: `input`
//!   its debug text, `value` the event as JSON (`{"Key":{…}}`, `"Close"`);
//! - `{"event":"stopped", …}` after `--seconds`; the window then stays as it
//!   is (a still window sends nothing more);
//! - an `end` line on stdin closes the window, as a person closing it does:
//!   the grant is revoked and the stream ends (`{"event":"ended"}`), but the
//!   session stays connected and keeps reporting any input still forwarded
//!   to it, so a harness can tell that none is;
//! - when stdin closes, the window closes (if `end` did not already) and the
//!   session leaves.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a qualification tool reports on stdout and fails on stderr"
)]

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use neighbour_wire::NeighbourMsg;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_media::realtime::{EncoderConfig, InputFormat, InputFrame, StreamCodec};
use tilo_relay_protocol::legacy_window::InputEvent;
use tilo_relay_protocol::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};
use tilo_relay_session::rooms::{
    ENTER_ROOM, EnteredRoom, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
};
use tilo_relay_session::{AttributedPresence, RelayClient};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

/// The app the window is listed as.
const APP: &str = "org.tilo.StreamQualification";
/// Number blocks: 16 bits of the frame number, then a 4-bit check.
const CODE_BITS: u32 = 20;
/// The shell's legacy-share defaults (`FlatpakShareConfig`, `flatpak_share`).
const BITRATE_BPS: u64 = 8_000_000;
const HARDWARE_FPS: u32 = 60;
const SOFTWARE_FPS: u32 = 30;
/// A workspace of the host's person that is no room: where this device asks
/// the host's own actions before it holds the room.
const HOME_WORKSPACE: u64 = 0x686f_6d65;

struct Args {
    dev_state: PathBuf,
    host: String,
    room: String,
    out: PathBuf,
    width: u32,
    height: u32,
    seconds: f64,
    fps: Option<u32>,
    surface: u64,
    codec: StreamCodec,
    /// Seconds into the stream at which the window takes `resize_to`.
    resize_at: Option<f64>,
    resize_to: Option<(u32, u32)>,
}

/// `WIDTHxHEIGHT`, each even and at least 320x64: what H.264 4:2:0 and the
/// number blocks need.
fn window_size(width: u32, height: u32) -> anyhow::Result<(u32, u32)> {
    if width < 320 || height < 64 || width % 2 == 1 || height % 2 == 1 {
        bail!("the window must be at least 320x64 and of even size");
    }
    Ok((width, height))
}

fn parse_size(text: &str) -> anyhow::Result<(u32, u32)> {
    let (width, height) = text.split_once('x').context("a size is WIDTHxHEIGHT")?;
    window_size(
        width.parse().context("width")?,
        height.parse().context("height")?,
    )
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        dev_state: PathBuf::new(),
        host: String::new(),
        room: String::new(),
        out: PathBuf::new(),
        width: 1280,
        height: 720,
        seconds: 10.0,
        fps: None,
        surface: 7,
        codec: StreamCodec::H264,
        resize_at: None,
        resize_to: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--dev-state" => parsed.dev_state = value.into(),
            "--host" => parsed.host = value,
            "--room" => parsed.room = value,
            "--out" => parsed.out = value.into(),
            "--width" => parsed.width = value.parse().context("--width")?,
            "--height" => parsed.height = value.parse().context("--height")?,
            "--seconds" => parsed.seconds = value.parse().context("--seconds")?,
            "--fps" => parsed.fps = Some(value.parse().context("--fps")?),
            "--surface" => parsed.surface = value.parse().context("--surface")?,
            "--codec" => {
                parsed.codec = StreamCodec::from_wire_label(&value)
                    .filter(|codec| {
                        matches!(
                            codec,
                            StreamCodec::H264 | StreamCodec::Vp9 | StreamCodec::Av1
                        )
                    })
                    .with_context(|| format!("--codec {value}: one of h264, vp9, av1"))?;
            }
            "--resize-at" => parsed.resize_at = Some(value.parse().context("--resize-at")?),
            "--resize-to" => parsed.resize_to = Some(parse_size(&value).context("--resize-to")?),
            other => bail!("unknown flag {other}"),
        }
    }
    if parsed.dev_state.as_os_str().is_empty()
        || parsed.host.is_empty()
        || parsed.room.is_empty()
        || parsed.out.as_os_str().is_empty()
    {
        bail!("--dev-state, --host, --room and --out are required");
    }
    window_size(parsed.width, parsed.height)?;
    if !(parsed.seconds > 0.0 && parsed.seconds <= 600.0) {
        bail!("--seconds must be in (0, 600]");
    }
    match (parsed.resize_at, parsed.resize_to) {
        (None, None) => {}
        (Some(at), Some(_)) if at > 0.0 && at < parsed.seconds => {}
        _ => bail!("--resize-at (inside --seconds) and --resize-to go together"),
    }
    Ok(parsed)
}

/// Wall-clock microseconds since the Unix epoch: the page reads the same
/// clock (`performance.timeOrigin + performance.now()`) on this host.
fn wall_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
        })
}

fn emit(line: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// The 4-bit check over a frame number's nibbles: a picture whose blocks do
/// not match it is a corrupt one, not a different frame.
#[must_use]
fn code_check(index: u16) -> u8 {
    let [high, low] = index.to_be_bytes();
    ((high >> 4) ^ (high & 0xF) ^ (low >> 4) ^ (low & 0xF) ^ 0xA) & 0xF
}

/// Where the number blocks sit in a picture `width` wide: their pitch, side
/// and the offset of the first from the top-left corner. The page decodes
/// them with the same geometry (`codeGeometry`).
#[must_use]
fn code_geometry(width: u32) -> (u32, u32, u32) {
    let pitch = width / (CODE_BITS + 1);
    (pitch, pitch.saturating_mul(4) / 5, pitch / 4)
}

/// The picture of the window without its caret and number: grey text lines
/// on white, with a mid-grey band behind the number row.
fn background(width: u32, height: u32) -> Vec<u8> {
    let (pitch, _, offset) = code_geometry(width);
    let band = offset.saturating_mul(2).saturating_add(pitch);
    let mut rgba = Vec::with_capacity(InputFormat::Rgba.frame_len(width, height).unwrap_or(0));
    for y in 0..height {
        for x in 0..width {
            let shade: u8 = if y < band {
                128
            } else if (y / 24) % 2 == 0 && x % 97 < 80 {
                90
            } else {
                240
            };
            rgba.extend_from_slice(&[shade, shade, shade.saturating_add(10), 255]);
        }
    }
    rgba
}

fn fill(rgba: &mut [u8], width: u32, rect: (u32, u32, u32, u32), shade: u8) {
    let (x0, y0, w, h) = rect;
    let pixels = |n: u32| usize::try_from(n).unwrap_or(usize::MAX).saturating_mul(4);
    let stride = pixels(width);
    for y in y0..y0.saturating_add(h) {
        let row = usize::try_from(y)
            .unwrap_or(usize::MAX)
            .saturating_mul(stride);
        let start = row.saturating_add(pixels(x0));
        let end = start.saturating_add(pixels(w));
        if let Some(pixels) = rgba.get_mut(start..end) {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[shade, shade, shade, 255]);
            }
        }
    }
}

/// Draw frame `index`'s number: 16 bits, most significant first, then the
/// check, one block each, white for 1 and black for 0.
fn draw_code(rgba: &mut [u8], width: u32, index: u16) {
    let (pitch, side, offset) = code_geometry(width);
    let bits = (u32::from(index) << 4) | u32::from(code_check(index));
    for bit in 0..CODE_BITS {
        let set = (bits >> (CODE_BITS - 1).saturating_sub(bit)) & 1 == 1;
        let rect = (
            offset.saturating_add(bit.saturating_mul(pitch)),
            offset,
            side,
            side,
        );
        fill(rgba, width, rect, if set { 235 } else { 16 });
    }
}

/// Frame `index`: the background, a caret bar that moves every frame (so
/// every frame is damaged and differs from the last) and the number.
fn frame(base: &[u8], width: u32, height: u32, index: u16) -> Vec<u8> {
    let mut rgba = base.to_vec();
    let (pitch, _, offset) = code_geometry(width);
    let top = offset.saturating_mul(2).saturating_add(pitch);
    let caret = u32::from(index)
        .saturating_mul(7)
        .checked_rem(width.saturating_sub(3))
        .unwrap_or(0);
    fill(
        &mut rgba,
        width,
        (caret, top, 3, height.saturating_sub(top)),
        20,
    );
    draw_code(&mut rgba, width, index);
    rgba
}

/// A device of the dev host's user: the root and host keys from
/// `dev-identity.bin` (`web_cli::dev_identity`), and a new device the root
/// certifies, as a person's laptop is.
fn identity(state: &std::path::Path) -> anyhow::Result<(SigningKey, DeviceCert, [u8; 32])> {
    let path = state.join("dev-identity.bin");
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let seed = |at: usize| -> anyhow::Result<[u8; 32]> {
        bytes
            .get(at.saturating_mul(32)..at.saturating_add(1).saturating_mul(32))
            .and_then(|seed| <[u8; 32]>::try_from(seed).ok())
            .with_context(|| format!("{} is not a dev identity", path.display()))
    };
    let root = SigningKey::from_bytes(&seed(0)?);
    let host = SigningKey::from_bytes(&seed(1)?).verifying_key().to_bytes();
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "stream qualification session".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).context("certify the session device")?;
    Ok((key, cert, host))
}

/// One room's presence, as this device: the relay does not stamp it.
struct Room {
    client: Arc<RelayClient>,
    workspace_id: u64,
    author: [u8; 32],
    codec: StreamCodec,
}

impl Room {
    fn publish(&self, message: &NeighbourMsg) -> anyhow::Result<()> {
        let payload = AttributedPresence {
            author: self.author,
            body: postcard::to_allocvec(message).context("encode presence")?,
        }
        .encode()
        .context("encode attributed presence")?;
        self.client
            .send(Envelope::Presence {
                workspace_id: self.workspace_id,
                payload,
            })
            .map_err(|_| anyhow::anyhow!("the relay session closed"))
    }

    /// Announce the window at `width`×`height`, as the share session does
    /// when it starts streaming a window and whenever the window's size
    /// changes.
    fn announce(&self, surface: u64, width: u32, height: u32) -> anyhow::Result<()> {
        self.publish(&NeighbourMsg::SurfaceStreamStart {
            surface,
            source_peer: tilo_peer_state::peer_handle(&self.author),
            width,
            height,
            codec: self.codec.wire_label().into(),
            ts: wall_us().saturating_mul(1000),
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
            app_id: APP.into(),
            tab: 0,
        })
    }

    /// Close the window: revoke the host's grant and end the stream.
    fn close_window(&self, surface: u64, host: &[u8; 32]) -> anyhow::Result<()> {
        self.publish(&NeighbourMsg::PermissionGrant {
            peer: tilo_peer_state::peer_handle(host),
            capability: format!("workspace-control:{surface}:revoked"),
        })?;
        self.publish(&NeighbourMsg::SurfaceStreamEnd {
            surface,
            source_peer: tilo_peer_state::peer_handle(&self.author),
            ts: wall_us().saturating_mul(1000),
        })
    }
}

/// The production encoder with the shell's legacy-share configuration, for a
/// window `width`×`height` in `codec`.
fn encoder(
    codec: StreamCodec,
    width: u32,
    height: u32,
) -> anyhow::Result<tilo_media::RealtimeEncoder> {
    let mut config = EncoderConfig::new(codec, width, height, HARDWARE_FPS);
    config.bitrate_bps = BITRATE_BPS;
    config.keyframe_interval = HARDWARE_FPS;
    config.input = InputFormat::Rgba;
    config.screen_content = true;
    tilo_media::RealtimeEncoder::new(&config)
        .map_err(|error| anyhow::anyhow!("no {} encoder: {error}", codec.wire_label()))
}

/// Print every input event the host forwards to this device, with the time
/// it arrived.
async fn report_input(client: Arc<RelayClient>) {
    while let Some(envelope) = client.recv().await {
        let Envelope::Presence { payload, .. } = envelope else {
            continue;
        };
        let arrival_us = wall_us();
        let Some(attributed) = AttributedPresence::decode(&payload) else {
            continue;
        };
        if let Ok(NeighbourMsg::RemoteInput { surface, event, .. }) =
            postcard::from_bytes(&attributed.body)
            && let Ok(input) = postcard::from_bytes::<InputEvent>(&event)
        {
            emit(&serde_json::json!({
                "event": "input", "arrivalUs": arrival_us, "surface": surface,
                "input": format!("{input:?}"), "value": input,
            }));
        }
    }
}

/// Lines from stdin on a thread of their own; the end of stdin is `None`.
fn stdin_lines() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// What the stream sent.
struct Sent {
    frames: u32,
    last_index: u16,
    elapsed: Duration,
}

/// Capture, encode and publish frames at `fps` for `seconds`. A frame whose
/// tick passed while the last one was being encoded is skipped, as the
/// share session's throttle skips it. With `--resize-at`, the window takes
/// its new size there: announced, then encoded from a keyframe by an
/// encoder for that size, as the share session restarts its encoder.
fn stream(
    room: &Room,
    args: &Args,
    mut encoder: tilo_media::RealtimeEncoder,
    fps: u32,
) -> anyhow::Result<Sent> {
    let mut log = std::io::BufWriter::new(
        std::fs::File::create(&args.out)
            .with_context(|| format!("create {}", args.out.display()))?,
    );
    let source_peer = tilo_peer_state::peer_handle(&room.author);
    let (mut width, mut height) = (args.width, args.height);
    let mut base = background(width, height);
    let mut resize = args.resize_at.zip(args.resize_to);
    let interval = Duration::from_secs(1)
        .checked_div(fps)
        .context("--fps must be above zero")?;
    let start = Instant::now();
    let total = Duration::from_secs_f64(args.seconds);
    // Each submitted frame's number and capture time, by its timestamp: the
    // encoder may hand a frame back with a later call.
    let mut pending = std::collections::VecDeque::new();
    let mut seq = 0_u64;
    let (mut tick, mut frames, mut last_index) = (0u32, 0u32, 0u16);
    while start.elapsed() < total {
        let due = interval.saturating_mul(tick);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        if let Some((at, size)) = resize
            && start.elapsed() >= Duration::from_secs_f64(at)
        {
            resize = None;
            (width, height) = size;
            room.announce(args.surface, width, height)?;
            encoder = self::encoder(args.codec, width, height)?;
            base = background(width, height);
            pending.clear();
        }
        let index = u16::try_from(frames).context("more than 65535 frames")?;
        let capture_us = wall_us();
        let picture = frame(&base, width, height, index);
        let encode_start = Instant::now();
        let pts = start.elapsed();
        pending.push_back((pts, index, capture_us));
        let units = encoder
            .encode(InputFrame::Packed(&picture), pts)
            .map_err(|error| anyhow::anyhow!("encode frame {index}: {error}"))?;
        let encode_us = u64::try_from(encode_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        for unit in units {
            while pending.front().is_some_and(|(at, ..)| *at < unit.pts) {
                pending.pop_front();
            }
            let Some((_, index, capture_us)) = pending.pop_front() else {
                continue;
            };
            room.publish(&NeighbourMsg::SurfaceFrame {
                surface: args.surface,
                source_peer,
                encoded_h264: unit.data.clone(),
                ts: capture_us.saturating_mul(1000),
                seq,
            })?;
            seq = seq.saturating_add(1);
            writeln!(
                log,
                "{}",
                serde_json::json!({"index": index, "captureUs": capture_us, "publishUs": wall_us(),
                    "encodeUs": encode_us, "bytes": unit.data.len(), "key": unit.keyframe,
                    "width": width, "height": height})
            )?;
            last_index = index;
        }
        frames = frames.saturating_add(1);
        // The next tick not yet due: a slow encode skips the ones it missed.
        let elapsed = start.elapsed().as_nanos();
        tick = u32::try_from(elapsed.checked_div(interval.as_nanos()).unwrap_or(0))
            .unwrap_or(u32::MAX)
            .saturating_add(1);
    }
    log.flush()?;
    Ok(Sent {
        frames,
        last_index,
        elapsed: start.elapsed(),
    })
}

/// Subscribe `client` to `workspace_id` and wait until the host admits it.
async fn subscribe(client: &RelayClient, workspace_id: u64) -> anyhow::Result<()> {
    client
        .send(Envelope::Subscribe { workspace_id })
        .map_err(|_| anyhow::anyhow!("the relay session closed"))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(envelope) = client.recv().await {
            match envelope {
                Envelope::Ready { .. } => return Ok(()),
                Envelope::NotAuthorised { .. } => bail!("the host refused the subscription"),
                _ => {}
            }
        }
        bail!("the relay session closed before the subscription was ready")
    })
    .await
    .context("the subscription was not ready within 30 s")?
}

/// Enter `room`, which the host governs, as one of its owner's devices: the
/// host's `room.enter` action, asked on a workspace of the person that is no
/// room, admits it.
async fn enter_room(
    endpoint: &iroh::Endpoint,
    addr: &iroh::EndpointAddr,
    cert: &DeviceCert,
    key: &SigningKey,
    host: &[u8; 32],
    room: &str,
) -> anyhow::Result<()> {
    let home = RelayClient::connect_to_pinned_host(
        endpoint.clone(),
        addr.clone(),
        cert.clone(),
        key.clone(),
        RevocationList::new(),
    )
    .await
    .context("connect to the host")?;
    subscribe(&home, HOME_WORKSPACE).await?;
    let mut argument = serde_json::Map::new();
    argument.insert("room".into(), room.into());
    let invoke = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.into(),
            address: ActionAddress::new(HOST_ROOMS_RESOURCE, ENTER_ROOM),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    });
    home.send(Envelope::RemoteAction {
        workspace_id: HOME_WORKSPACE,
        version: REMOTE_ACTION_ENVELOPE_VERSION,
        to_device_public_key: *host,
        from_device_public_key: [0; 32],
        from_user_public_key: [0; 32],
        frame: RemoteActionFrame::new(invoke)
            .encode()
            .map_err(|error| anyhow::anyhow!("encode the action: {error:?}"))?,
    })
    .map_err(|_| anyhow::anyhow!("the relay session closed"))?;
    let answer = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(envelope) = home.recv().await {
            match envelope {
                Envelope::RemoteAction { frame, .. } => {
                    return RemoteActionFrame::decode(&frame)
                        .map(|frame| frame.message)
                        .map_err(|error| anyhow::anyhow!("the host's answer: {error:?}"));
                }
                Envelope::RemoteActionUndeliverable { reason, .. } => {
                    bail!("the host did not take the action: {reason}")
                }
                _ => {}
            }
        }
        bail!("the relay session closed before the host answered")
    })
    .await
    .context("the host did not answer within 30 s")??;
    let RemoteActionMessage::Receipt(receipt) = answer else {
        bail!("the host answered the action with no receipt");
    };
    // Admitted: the device may subscribe to the room's workspace now. The
    // invitation it is also handed holds the room for a device that keeps
    // it; a session that streams one window needs no more than admission.
    let _entered: EnteredRoom = match receipt.status {
        ReceiptStatus::Completed { output } => {
            serde_json::from_value(output.value.unwrap_or_default())
                .context("the host's answer is no entered room")?
        }
        other => bail!("the host refused to admit this device: {other:?}"),
    };
    // Dropped, not closed: closing would close the endpoint the room's
    // session goes on to use.
    drop(home);
    Ok(())
}

async fn run(args: Args) -> anyhow::Result<()> {
    let (key, cert, host) = identity(&args.dev_state)?;
    let workspace_id = tilo_relay_protocol::room_workspace_id(&args.room)
        .map_err(|_| anyhow::anyhow!("--room must be a uuid"))?;
    let addr: iroh::EndpointAddr =
        serde_json::from_str(&args.host).context("--host is the dev host's irohAddr JSON")?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .context("bind the session's endpoint")?;
    // In a room the host governs it bridges only a member's windows
    // (`legacy_writer_role`): the session enters the room as one of its
    // owner's devices, as a person's session does when it opens the room's
    // workspace.
    enter_room(&endpoint, &addr, &cert, &key, &host, &args.room)
        .await
        .context("enter the room")?;
    let client = Arc::new(
        RelayClient::connect_to_pinned_host(
            endpoint,
            addr,
            cert.clone(),
            key,
            RevocationList::new(),
        )
        .await
        .context("connect to the host")?,
    );
    subscribe(&client, workspace_id).await?;
    let encoder = encoder(args.codec, args.width, args.height)?;
    let element = encoder.element();
    let fps = args.fps.unwrap_or_else(|| {
        if element.is_hardware() {
            HARDWARE_FPS
        } else {
            SOFTWARE_FPS
        }
    });
    let room = Room {
        client: Arc::clone(&client),
        workspace_id,
        author: cert.device_public_key,
        codec: args.codec,
    };
    let input = tokio::spawn(report_input(Arc::clone(&client)));
    emit(&serde_json::json!({
        "event": "ready", "device": hex::encode(cert.device_public_key),
        "encoder": element.describe(), "hardware": element.is_hardware(), "fps": fps,
        "codec": args.codec.wire_label(),
        "width": args.width, "height": args.height, "surface": args.surface, "enteredRoom": true,
    }));
    let mut stdin = stdin_lines();
    loop {
        match stdin.recv().await.as_deref().map(str::trim) {
            Some("start") => break,
            Some(_) => {}
            None => bail!("stdin closed before start"),
        }
    }
    let start_us = wall_us();
    room.publish(&NeighbourMsg::PermissionGrant {
        peer: tilo_peer_state::peer_handle(&host),
        capability: format!("workspace-control:{}:granted", args.surface),
    })?;
    room.announce(args.surface, args.width, args.height)?;
    emit(&serde_json::json!({"event": "started", "startUs": start_us}));
    let sent = tokio::task::block_in_place(|| stream(&room, &args, encoder, fps))?;
    emit(&serde_json::json!({
        "event": "stopped", "frames": sent.frames, "lastIndex": sent.last_index,
        "seconds": sent.elapsed.as_secs_f64(), "stopUs": wall_us(),
    }));
    // The window stays open, and still, until `end` or the end of stdin.
    let mut open = true;
    while let Some(line) = stdin.recv().await {
        if open && line.trim() == "end" {
            open = false;
            room.close_window(args.surface, &host)?;
            emit(&serde_json::json!({"event": "ended", "endUs": wall_us()}));
        }
    }
    if open {
        room.close_window(args.surface, &host)?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    input.abort();
    let _ = input.await;
    drop(room);
    if let Ok(client) = Arc::try_unwrap(client) {
        client.close().await;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("legacy_stream_source: {error:#}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("legacy_stream_source: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("legacy_stream_source: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_number_reads_back_from_its_picture() {
        let (width, height) = (640, 96);
        let base = background(width, height);
        let (pitch, side, offset) = code_geometry(width);
        for index in [0u16, 1, 2, 255, 4097, 0xBEEF, u16::MAX] {
            let picture = frame(&base, width, height, index);
            let mut bits = 0u32;
            for bit in 0..CODE_BITS {
                let x = offset
                    .saturating_add(bit.saturating_mul(pitch))
                    .saturating_add(side / 2);
                let y = offset.saturating_add(side / 2);
                let at =
                    usize::try_from(y.saturating_mul(width).saturating_add(x).saturating_mul(4))
                        .unwrap();
                bits = (bits << 1) | u32::from(picture[at] > 128);
            }
            assert_eq!(bits >> 4, u32::from(index));
            assert_eq!(bits & 0xF, u32::from(code_check(index)));
        }
    }

    #[test]
    fn the_check_tells_a_neighbouring_number_from_a_corrupt_one() {
        // One flipped number bit always changes the check.
        for index in [0u16, 7, 300, 0x1234] {
            for bit in 0..16 {
                assert_ne!(code_check(index), code_check(index ^ (1 << bit)));
            }
        }
    }

    #[test]
    fn a_resize_names_an_even_window_the_number_blocks_fit() {
        assert_eq!(parse_size("800x448").unwrap(), (800, 448));
        for refused in ["801x448", "800x447", "318x64", "800", "x448", "wide x tall"] {
            assert!(parse_size(refused).is_err(), "{refused} was accepted");
        }
    }

    #[test]
    fn input_reaches_the_harness_as_the_json_it_matches() {
        let key = InputEvent::Key {
            keysym: 30,
            pressed: true,
            modifiers: 0,
        };
        assert_eq!(
            serde_json::to_value(&key).unwrap(),
            serde_json::json!({"Key": {"keysym": 30, "pressed": true, "modifiers": 0}})
        );
        let pasted = InputEvent::ClipboardText {
            text: "line 1\nzwei ✓".into(),
        };
        assert_eq!(
            serde_json::to_value(&pasted).unwrap(),
            serde_json::json!({"ClipboardText": {"text": "line 1\nzwei ✓"}})
        );
    }
}
