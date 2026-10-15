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
//! ```
//!
//! `--codec` picks the stream's codec (H.264 by default). VP9 and AV1 are what
//! a session without an H.264 encoder (no hardware one, no codec pack) streams
//! once every browser member decodes them.
//!
//! `--dev-state` is `tilo-relay dev --state`: its `dev-identity.bin` holds
//! the user root and the host device's keys. Protocol on stdio, one JSON
//! object per stdout line:
//!
//! - `{"event":"ready", …}` once subscribed, with the encoder chosen;
//! - a `start` line on stdin starts the stream: `{"event":"started"}`, then
//!   one line per frame to `--out` (`index`, `captureUs`, `encodeUs`, `bytes`,
//!   `key`, wall-clock microseconds since the Unix epoch);
//! - `{"event":"input", …}` for every input event the host forwards;
//! - `{"event":"stopped", …}` after `--seconds`; the window then stays as it
//!   is (a still window sends nothing more) until stdin closes, when the
//!   grant is revoked and the stream ends.

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
use tilo_media::realtime::{EncoderConfig, InputFormat, InputFrame, StreamCodec};
use tilo_relay_protocol::legacy_window::InputEvent;
use tilo_relay_protocol::transport::Envelope;
use tilo_relay_session::rooms::{RoomAdminClient, RoomAdminError, RoomCommand};
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
    if parsed.width < 320 || parsed.height < 64 || parsed.width % 2 == 1 || parsed.height % 2 == 1 {
        bail!("the window must be at least 320x64 and of even size");
    }
    if !(parsed.seconds > 0.0 && parsed.seconds <= 600.0) {
        bail!("--seconds must be in (0, 600]");
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
    (pitch, pitch * 4 / 5, pitch / 4)
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
    let stride = usize::try_from(width).unwrap_or(0) * 4;
    for y in y0..y0.saturating_add(h) {
        let row = usize::try_from(y).unwrap_or(0) * stride;
        let start = row + usize::try_from(x0).unwrap_or(0) * 4;
        let end = start + usize::try_from(w).unwrap_or(0) * 4;
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
        let set = (bits >> (CODE_BITS - 1 - bit)) & 1 == 1;
        let rect = (offset + bit * pitch, offset, side, side);
        fill(rgba, width, rect, if set { 235 } else { 16 });
    }
}

/// Frame `index`: the background, a caret bar that moves every frame (so
/// every frame is damaged and differs from the last) and the number.
fn frame(base: &[u8], width: u32, height: u32, index: u16) -> Vec<u8> {
    let mut rgba = base.to_vec();
    let (pitch, _, offset) = code_geometry(width);
    let top = offset.saturating_mul(2).saturating_add(pitch);
    let caret = u32::from(index).saturating_mul(7) % width.saturating_sub(3);
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
            .get(at * 32..(at + 1) * 32)
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
                "input": format!("{input:?}"),
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
/// share session's throttle skips it.
fn stream(
    room: &Room,
    args: &Args,
    encoder: &mut tilo_media::RealtimeEncoder,
    fps: u32,
) -> anyhow::Result<Sent> {
    let mut log = std::io::BufWriter::new(
        std::fs::File::create(&args.out)
            .with_context(|| format!("create {}", args.out.display()))?,
    );
    let source_peer = tilo_peer_state::peer_handle(&room.author);
    let base = background(args.width, args.height);
    let interval = Duration::from_secs(1) / fps;
    let start = Instant::now();
    let total = Duration::from_secs_f64(args.seconds);
    // Each submitted frame's number and capture time, by its timestamp: the
    // encoder may hand a frame back with a later call.
    let mut pending = std::collections::VecDeque::new();
    let (mut tick, mut frames, mut last_index) = (0u32, 0u32, 0u16);
    while start.elapsed() < total {
        let due = interval * tick;
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        let index = u16::try_from(frames).context("more than 65535 frames")?;
        let capture_us = wall_us();
        let picture = frame(&base, args.width, args.height, index);
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
            })?;
            writeln!(
                log,
                "{}",
                serde_json::json!({"index": index, "captureUs": capture_us, "publishUs": wall_us(),
                    "encodeUs": encode_us, "bytes": unit.data.len(), "key": unit.keyframe})
            )?;
            last_index = index;
        }
        frames = frames.saturating_add(1);
        // The next tick not yet due: a slow encode skips the ones it missed.
        let elapsed = start.elapsed().as_nanos();
        tick = u32::try_from(elapsed / interval.as_nanos().max(1))
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
    // workspace. A room the host does not govern has no members to enter.
    let entered = match RoomAdminClient::new(endpoint.clone(), cert.clone())
        .send(
            addr.clone(),
            RoomCommand::Enter {
                room: args.room.clone(),
            },
        )
        .await
    {
        Ok(_) => true,
        Err(RoomAdminError::Refused(reason)) if reason.contains("does not govern") => false,
        Err(error) => return Err(error).context("enter the room"),
    };
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
    client
        .send(Envelope::Subscribe { workspace_id })
        .map_err(|_| anyhow::anyhow!("the relay session closed"))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(envelope) = client.recv().await {
            if matches!(envelope, Envelope::Ready { .. }) {
                return Ok(());
            }
        }
        bail!("the relay session closed before the subscription was ready")
    })
    .await
    .context("the subscription was not ready within 30 s")??;
    let mut config = EncoderConfig::new(args.codec, args.width, args.height, HARDWARE_FPS);
    config.bitrate_bps = BITRATE_BPS;
    config.keyframe_interval = HARDWARE_FPS;
    config.input = InputFormat::Rgba;
    config.screen_content = true;
    let mut encoder = tilo_media::RealtimeEncoder::new(&config)
        .map_err(|error| anyhow::anyhow!("no {} encoder: {error}", args.codec.wire_label()))?;
    let element = encoder.element();
    let fps = args.fps.unwrap_or(if element.is_hardware() {
        HARDWARE_FPS
    } else {
        SOFTWARE_FPS
    });
    let room = Room {
        client: Arc::clone(&client),
        workspace_id,
        author: cert.device_public_key,
    };
    let input = tokio::spawn(report_input(Arc::clone(&client)));
    emit(&serde_json::json!({
        "event": "ready", "device": hex::encode(cert.device_public_key),
        "encoder": element.describe(), "hardware": element.is_hardware(), "fps": fps,
        "codec": args.codec.wire_label(),
        "width": args.width, "height": args.height, "surface": args.surface, "enteredRoom": entered,
    }));
    let mut stdin = stdin_lines();
    loop {
        match stdin.recv().await.as_deref().map(str::trim) {
            Some("start") => break,
            Some(_) => {}
            None => bail!("stdin closed before start"),
        }
    }
    let host_peer = tilo_peer_state::peer_handle(&host);
    let source_peer = tilo_peer_state::peer_handle(&cert.device_public_key);
    let start_us = wall_us();
    room.publish(&NeighbourMsg::PermissionGrant {
        peer: host_peer,
        capability: format!("workspace-control:{}:granted", args.surface),
    })?;
    room.publish(&NeighbourMsg::SurfaceStreamStart {
        surface: args.surface,
        source_peer,
        width: args.width,
        height: args.height,
        codec: args.codec.wire_label().into(),
        ts: start_us.saturating_mul(1000),
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 0,
        app_id: APP.into(),
        tab: 0,
    })?;
    emit(&serde_json::json!({"event": "started", "startUs": start_us}));
    let sent = tokio::task::block_in_place(|| stream(&room, &args, &mut encoder, fps))?;
    emit(&serde_json::json!({
        "event": "stopped", "frames": sent.frames, "lastIndex": sent.last_index,
        "seconds": sent.elapsed.as_secs_f64(), "stopUs": wall_us(),
    }));
    while stdin.recv().await.is_some() {}
    room.publish(&NeighbourMsg::PermissionGrant {
        peer: host_peer,
        capability: format!("workspace-control:{}:revoked", args.surface),
    })?;
    room.publish(&NeighbourMsg::SurfaceStreamEnd {
        surface: args.surface,
        source_peer,
        ts: wall_us().saturating_mul(1000),
    })?;
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
                let (x, y) = (offset + bit * pitch + side / 2, offset + side / 2);
                let at = usize::try_from((y * width + x) * 4).unwrap();
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
}
