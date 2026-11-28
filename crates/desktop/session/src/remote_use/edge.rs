//! The hardware at the edges of a remote use on this machine: the camera, microphone and scanner
//! captured through the OS media programs, the virtual camera (a v4l2loopback device labelled
//! [`REMOTE_CAMERA_LABEL`]) and microphone (a `PipeWire` loopback source) in place for another
//! device's, the scanned documents saved where the person keeps documents, and the game
//! controllers plugged in here. Only trusted programs installed with the system are run.

use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;

use tilo_workspace_service::workspaces::person_workspace::remote_use::{
    AudioFormat, DocumentFormat, Profile, VideoFormat,
};

use super::engine::{Capture, Edge, PadSource, Unit, VirtualDevice};
use super::wire::unit_bound;

/// Where the trusted programs are installed.
const TOOL_DIRECTORIES: [&str; 2] = ["/usr/bin", "/usr/libexec/tilo"];
/// The label of the v4l2loopback device a remote camera is played into.
pub(crate) const REMOTE_CAMERA_LABEL: &str = "Tilo Remote Camera";
/// The most units a capture keeps for the link: a live source past this drops the newest rather
/// than fall behind.
const KEPT_UNITS: usize = 8;

/// The edges of this machine.
pub(crate) struct Machine;

impl Edge for Machine {
    fn capture(&self, profile: &Profile) -> Result<Box<dyn Capture>, String> {
        let (command, framing) = capture_command(profile)?;
        Ok(Box::new(Running::start(command, framing)?))
    }

    fn virtual_device(
        &self,
        label: &str,
        profile: &Profile,
    ) -> Result<Box<dyn VirtualDevice>, String> {
        match profile {
            Profile::Camera {
                format,
                width,
                height,
                ..
            } => virtual_camera(*format, *width, *height),
            Profile::Microphone {
                format,
                sample_rate,
                channels,
                ..
            } => virtual_microphone(*format, *sample_rate, *channels),
            Profile::Scanner { format, .. } => Ok(Box::new(Inbox {
                label: label.to_owned(),
                format: *format,
            })),
            Profile::Controller => Err("a controller takes its place as a virtual pad".to_owned()),
        }
    }

    fn pad(&self, label: &str) -> Result<Box<dyn PadSource>, String> {
        let catalog = os_actions::LinuxPhysicalGamepadCatalog::default();
        let found = catalog
            .discover()
            .map_err(|error| format!("the game controllers here could not be listed: {error}"))?
            .into_iter()
            .find(|info| info.name == label)
            .ok_or_else(|| format!("no game controller called {label} is plugged in here"))?;
        let source = catalog
            .open(&found.id)
            .map_err(|error| format!("the game controller could not be opened: {error}"))?;
        Ok(Box::new(Pad(source)))
    }
}

/// The names of the game controllers plugged in here.
pub(crate) fn controllers() -> Vec<String> {
    os_actions::LinuxPhysicalGamepadCatalog::default()
        .discover()
        .map(|found| found.into_iter().map(|info| info.name).collect())
        .unwrap_or_default()
}

/// The trusted program `name`, when it is installed.
fn tool(name: &str) -> Result<PathBuf, String> {
    TOOL_DIRECTORIES
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

/// Every video device and its label, in the kernel's order.
fn video_devices() -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut devices: Vec<(PathBuf, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let label = std::fs::read_to_string(entry.path().join("name")).ok()?;
            Some((
                Path::new("/dev").join(entry.file_name()),
                label.trim().to_owned(),
            ))
        })
        .collect();
    devices.sort();
    devices
}

/// How a capture's output splits into units.
#[derive(Clone, Copy)]
enum Framing {
    /// Raw frames or sound of this many bytes each.
    Fixed(usize),
    /// JPEG frames, one after another.
    Jpeg,
    /// The whole output is one scanned page.
    Document,
}

/// The program that captures `profile` here, and how its output splits.
fn capture_command(profile: &Profile) -> Result<(Command, Framing), String> {
    match profile {
        Profile::Camera {
            format,
            width,
            height,
            frames_per_second,
        } => {
            let camera = video_devices()
                .into_iter()
                .find(|(_, label)| !label.starts_with(REMOTE_CAMERA_LABEL))
                .map(|(device, _)| device)
                .ok_or("there is no camera here")?;
            let mut command = Command::new(tool("gst-launch-1.0")?);
            command
                .arg("-q")
                .arg("v4l2src")
                .arg(format!("device={}", camera.display()))
                .args([
                    "!",
                    "videoconvert",
                    "!",
                    "videoscale",
                    "!",
                    "videorate",
                    "!",
                ]);
            let raw = |pixels: &str| {
                format!(
                    "video/x-raw,format={pixels},width={width},height={height},framerate={frames_per_second}/1"
                )
            };
            let framing = match format {
                VideoFormat::Nv12 => {
                    command.arg(raw("NV12"));
                    Framing::Fixed(usize::try_from(unit_bound(profile)).unwrap_or(usize::MAX))
                }
                VideoFormat::Rgba8 => {
                    command.arg(raw("RGBA"));
                    Framing::Fixed(usize::try_from(unit_bound(profile)).unwrap_or(usize::MAX))
                }
                VideoFormat::Mjpeg => {
                    command.arg(raw("I420")).args(["!", "jpegenc"]);
                    Framing::Jpeg
                }
            };
            command.args(["!", "fdsink", "fd=1", "sync=false"]);
            Ok((command, framing))
        }
        Profile::Microphone {
            format,
            sample_rate,
            channels,
            ..
        } => {
            let mut command = Command::new(tool("pw-record")?);
            command
                .arg("--format")
                .arg(pw_format(*format))
                .arg("--rate")
                .arg(sample_rate.to_string())
                .arg("--channels")
                .arg(channels.to_string())
                .arg("-");
            Ok((
                command,
                Framing::Fixed(usize::try_from(unit_bound(profile)).unwrap_or(usize::MAX)),
            ))
        }
        Profile::Scanner { format, .. } => {
            let mut command = Command::new(tool("scanimage")?);
            command.arg(match format {
                DocumentFormat::Jpeg => "--format=jpeg",
                DocumentFormat::Png => "--format=png",
                DocumentFormat::Pdf => "--format=pdf",
            });
            Ok((command, Framing::Document))
        }
        Profile::Controller => Err("a controller is read, not captured".to_owned()),
    }
}

const fn pw_format(format: AudioFormat) -> &'static str {
    match format {
        AudioFormat::PcmS16Le => "s16",
        AudioFormat::PcmF32Le => "f32",
    }
}

/// A capture program running, its output read into units on a thread of its own.
struct Running {
    child: Child,
    units: mpsc::Receiver<Unit>,
}

impl Running {
    fn start(mut command: Command, framing: Framing) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("the capture did not start: {error}"))?;
        let stdout = child.stdout.take().ok_or("the capture has no output")?;
        let (sender, units) = mpsc::sync_channel(KEPT_UNITS);
        std::thread::Builder::new()
            .name("tilo-remote-capture".into())
            .spawn(move || read_units(stdout, framing, &sender))
            .map_err(|error| format!("the capture's reader did not start: {error}"))?;
        Ok(Self { child, units })
    }
}

impl Capture for Running {
    fn take(&mut self) -> Vec<Unit> {
        self.units.try_iter().take(KEPT_UNITS).collect()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read `source` into units until it ends. A unit the link has no room for is dropped.
fn read_units(mut source: impl Read, framing: Framing, sender: &mpsc::SyncSender<Unit>) {
    match framing {
        Framing::Fixed(bytes) if bytes > 0 => {
            let mut frame = vec![0; bytes];
            while source.read_exact(&mut frame).is_ok() {
                if matches!(
                    sender.try_send(Unit::Frame(frame.clone())),
                    Err(mpsc::TrySendError::Disconnected(_))
                ) {
                    return;
                }
            }
        }
        Framing::Fixed(_) => {}
        Framing::Jpeg => {
            let mut held = Vec::new();
            let mut buffer = vec![0; 64 * 1024];
            while let Ok(read) = source.read(&mut buffer) {
                let Some(read) = buffer.get(..read).filter(|read| !read.is_empty()) else {
                    return;
                };
                held.extend_from_slice(read);
                // A JPEG ends at its end-of-image marker, which its entropy-coded data never
                // carries unescaped.
                while let Some(end) = held.windows(2).position(|pair| pair == [0xFF, 0xD9]) {
                    let frame: Vec<u8> = held.drain(..end.saturating_add(2)).collect();
                    if matches!(
                        sender.try_send(Unit::Frame(frame)),
                        Err(mpsc::TrySendError::Disconnected(_))
                    ) {
                        return;
                    }
                }
            }
        }
        Framing::Document => {
            let mut page = Vec::new();
            if source.read_to_end(&mut page).is_ok() && !page.is_empty() {
                let _ = sender.send(Unit::Page {
                    bytes: page,
                    last: true,
                });
            }
        }
    }
}

/// The virtual camera: frames written to the v4l2loopback device labelled
/// [`REMOTE_CAMERA_LABEL`], which every app here lists as a camera.
struct Camera(std::fs::File);

fn virtual_camera(
    format: VideoFormat,
    width: u16,
    height: u16,
) -> Result<Box<dyn VirtualDevice>, String> {
    let device = video_devices()
        .into_iter()
        .find(|(_, label)| label.starts_with(REMOTE_CAMERA_LABEL))
        .map(|(device, _)| device)
        .ok_or("this device has no virtual camera to take its place")?;
    let pixels = match format {
        VideoFormat::Nv12 => "NV12",
        VideoFormat::Rgba8 => "RGB4",
        VideoFormat::Mjpeg => "MJPG",
    };
    let set = Command::new(tool("v4l2-ctl")?)
        .arg("--device")
        .arg(&device)
        .arg("--set-fmt-video-out")
        .arg(format!(
            "width={width},height={height},pixelformat={pixels}"
        ))
        .status()
        .map_err(|error| format!("the virtual camera could not be set up: {error}"))?;
    if !set.success() {
        return Err("the virtual camera did not take the camera's format".to_owned());
    }
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&device)
        .map_err(|error| format!("the virtual camera could not be opened: {error}"))?;
    Ok(Box::new(Camera(writer)))
}

impl VirtualDevice for Camera {
    fn frame(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.write_all(bytes).map_err(|error| error.to_string())
    }

    fn document(&mut self, _pages: Vec<Vec<u8>>) -> Result<(), String> {
        Err("a camera takes frames".to_owned())
    }
}

/// The virtual microphone: a `PipeWire` source every app here lists, fed through a loopback.
struct Microphone {
    loopback: Child,
    writer: Child,
    input: ChildStdin,
}

fn virtual_microphone(
    format: AudioFormat,
    sample_rate: u32,
    channels: u8,
) -> Result<Box<dyn VirtualDevice>, String> {
    let suffix = hex::encode(tilo_network_actor::fabric::nonzero::<8>());
    let sink = format!("tilo.remote-mic-sink.{suffix}");
    let source = format!("tilo.remote-mic-source.{suffix}");
    let mut loopback = Command::new(tool("pw-loopback")?)
        .arg(format!(
            "--capture-props={{ node.name=\"{sink}\" media.class=\"Audio/Sink\" }}"
        ))
        .arg(format!(
            "--playback-props={{ node.name=\"{source}\" node.description=\"Tilo remote microphone\" media.class=\"Audio/Source\" }}"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("the virtual microphone did not start: {error}"))?;
    let writer = Command::new(tool("pw-cat")?)
        .arg("--playback")
        .arg("--target")
        .arg(&sink)
        .arg("--format")
        .arg(pw_format(format))
        .arg("--rate")
        .arg(sample_rate.to_string())
        .arg("--channels")
        .arg(channels.to_string())
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut writer = match writer {
        Ok(writer) => writer,
        Err(error) => {
            let _ = loopback.kill();
            return Err(format!(
                "the virtual microphone's feed did not start: {error}"
            ));
        }
    };
    let Some(input) = writer.stdin.take() else {
        let _ = writer.kill();
        let _ = loopback.kill();
        return Err("the virtual microphone's feed takes no input".to_owned());
    };
    Ok(Box::new(Microphone {
        loopback,
        writer,
        input,
    }))
}

impl VirtualDevice for Microphone {
    fn frame(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.input
            .write_all(bytes)
            .map_err(|error| error.to_string())
    }

    fn document(&mut self, _pages: Vec<Vec<u8>>) -> Result<(), String> {
        Err("a microphone takes sound".to_owned())
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        let _ = self.writer.kill();
        let _ = self.loopback.kill();
        let _ = self.writer.wait();
        let _ = self.loopback.wait();
    }
}

/// Where scanned documents land: the person's documents, under `Scans`, one file per page.
struct Inbox {
    label: String,
    format: DocumentFormat,
}

impl VirtualDevice for Inbox {
    fn frame(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("a scanner sends documents".to_owned())
    }

    fn document(&mut self, pages: Vec<Vec<u8>>) -> Result<(), String> {
        let home = std::env::var_os("HOME").ok_or("the person has no home here")?;
        let folder = Path::new(&home).join("Documents").join("Scans");
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let extension = match self.format {
            DocumentFormat::Jpeg => "jpg",
            DocumentFormat::Png => "png",
            DocumentFormat::Pdf => "pdf",
        };
        let stamp = tilo_clock::unix_now_ns() / 1_000_000;
        let name: String = self
            .label
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .collect();
        for (number, page) in (1_usize..).zip(&pages) {
            let file = folder.join(format!("{} {stamp} {number}.{extension}", name.trim()));
            std::fs::write(&file, page).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// A game controller plugged in here.
struct Pad(os_actions::LinuxPhysicalGamepadSource);

impl PadSource for Pad {
    fn take(&mut self) -> Vec<os_actions::ForwardedGamepadEvent> {
        self.0
            .drain(64)
            .map(|drained| drained.events)
            .unwrap_or_default()
    }
}
