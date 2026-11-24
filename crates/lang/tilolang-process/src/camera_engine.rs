//! Camera's engine work, as a package reaches it: the cameras this device has, the picture a
//! camera shows, a photo or a clip kept from it, the codes a picture holds, and the document
//! scanner's page, text and PDF.
//!
//! The app runs sandboxed with no camera, no scanner and no decoder, so it asks by name. What
//! the engine writes back is kept in the workspace's content store and named by its address,
//! which the app shows as the file it is.
//!
//! The camera is opened once and drained by a thread of its own: a picture asked for is the
//! newest frame that thread took, so a viewfinder asking again and again costs a frame's copy,
//! not a pipeline build. A recording is that same thread's frames fed to the clip recorder, so
//! the app never carries pixels.
//!
//! Answers now (`engine.call`, `engine.picture`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `cameras` | `{}` | `{devices, prompt}` |
//! | `frame` | `{device, width, height}` | a file: the newest frame, kept small |
//! | `still` | `{device, width, height, format}` | a file: the frame as the photo is saved |
//! | `codes` | `{url, width}` | `{codes: [{text, corners}]}` |
//! | `ocr` | `{url, language}` | `{text, words, deskew}` |
//! | `pages_pdf` | `{pages, name}` | `{url}` |
//!
//! Jobs (`engine.start`, `engine.status`, `engine.cancel`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `scan` | `{scanner, resolution, colour, source, name}` | `{job}`; done with the page's `url` |
//! | `record` | `{device, width, height}` | `{job}`; `stop-record` finishes it, done with its `url` |

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;
use tilo_image_codec::{DecodedImage, Encoding};

use tilolang_tilo::instance::Platform;

/// The one app this device answers camera work for.
pub const APP: &str = "so.tilo.camera";

/// The largest picture a still is kept at, in pixels a side.
const MAX_STILL_EDGE: u32 = 4096;
/// The width a viewfinder picture is kept at: enough to fill a window, small enough to keep at
/// the rate a viewfinder asks.
const PREVIEW_WIDTH: u32 = 480;
/// How long a request waits for a camera it just opened to show its first frame.
const FIRST_FRAME: std::time::Duration = std::time::Duration::from_secs(5);
/// How often the camera thread looks for a frame while it waits.
const POLL: std::time::Duration = std::time::Duration::from_millis(10);
/// The codes one picture may hold; a poster wall is no code.
const MAX_CODES: usize = 16;

/// One `engine.call` for this device's app, or a refusal.
///
/// # Errors
///
/// The op is unknown, an input does not read, or the work cannot be done.
pub fn call(platform: &dyn Platform, app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    match op {
        "cameras" => Ok(cameras()),
        "frame" => frame(platform, input),
        "still" => still(platform, input),
        "codes" => codes(platform, input),
        "ocr" => ocr(platform, input),
        "pages_pdf" => pages_pdf(platform, input),
        "stop-record" => stop_record(input),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// One `engine.start`: a document page or a recording, answered with the job's id.
///
/// # Errors
///
/// Not this device's app, or the input does not read.
pub fn start(platform: &dyn Platform, app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    match op {
        "scan" => scan(platform, input),
        "record" => record(input),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// How far a job has got, as JSON: its state, and what it made or why it failed.
///
/// # Errors
///
/// No such job on this device.
pub fn status(platform: &dyn Platform, job: &str) -> Result<String, String> {
    let state = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .get(job)
        .cloned();
    let Some(state) = state else {
        return Err(format!("no job `{job}` runs on this device"));
    };
    state.settle(platform);
    if state.stopped.load(Ordering::Relaxed) {
        return Ok(serde_json::json!({ "state": "cancelled", "url": "", "error": "" }).to_string());
    }
    if let Some(error) = state.error.lock().ok().and_then(|error| error.clone()) {
        return Ok(serde_json::json!({ "state": "failed", "url": "", "error": error }).to_string());
    }
    match state.url.lock().ok().and_then(|url| url.clone()) {
        Some(url) => {
            Ok(serde_json::json!({ "state": "done", "url": url, "error": "" }).to_string())
        }
        None => Ok(serde_json::json!({ "state": "running", "url": "", "error": "" }).to_string()),
    }
}

/// Stop a job: false when it is not this device's or it had finished.
pub fn cancel(job: &str) -> bool {
    let Ok(known) = jobs().lock() else {
        return false;
    };
    let Some(state) = known.get(job) else {
        return false;
    };
    if state.finished.load(Ordering::Relaxed) {
        return false;
    }
    state.stopped.store(true, Ordering::Relaxed);
    if state.recording.load(Ordering::Relaxed)
        && let Some(handle) = keeper()
        && let Ok(handle) = handle.lock()
    {
        let (reply, _answered) = std::sync::mpsc::channel();
        let _ = handle.send(Control::CancelRecord { reply });
    }
    true
}

/// A page the scanner answered: its name, and the bytes its receiver keeps.
type Scan = (String, Receiver<Result<Vec<u8>, String>>);

/// One job in flight, and what it made.
struct Job {
    finished: AtomicBool,
    stopped: AtomicBool,
    /// Whether the job is a recording the camera thread holds.
    recording: AtomicBool,
    error: Mutex<Option<String>>,
    url: Mutex<Option<String>>,
    /// A page the scanner answered, kept as the job's file on the next ask.
    scan: Mutex<Option<Scan>>,
    /// A clip the camera thread answered: what it is called, and its bytes with its duration.
    clip: Mutex<Option<Clip>>,
}

struct Clip {
    name: String,
    bytes: Vec<u8>,
}

impl Job {
    /// Keep what a scanner or a recording answered, once: the file the app shows.
    fn settle(&self, platform: &dyn Platform) {
        self.take_scan(platform);
        self.take_clip(platform);
    }

    fn take_scan(&self, platform: &dyn Platform) {
        let Ok(mut slot) = self.scan.lock() else {
            return;
        };
        let Some((name, rx)) = slot.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(png)) => {
                match platform.keep_content(name, "image/png", &png) {
                    Ok(kept) => {
                        if let Ok(mut url) = self.url.lock() {
                            *url = Some(format!("tilo-blob://{}", kept.hash));
                        }
                    }
                    Err(error) => {
                        if let Ok(mut error_slot) = self.error.lock() {
                            *error_slot = Some(error);
                        }
                    }
                }
                *slot = None;
            }
            Ok(Err(error)) => {
                if let Ok(mut error_slot) = self.error.lock() {
                    *error_slot = Some(error);
                }
                *slot = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if let Ok(mut error_slot) = self.error.lock() {
                    *error_slot = Some("the scanner did not answer".to_owned());
                }
                *slot = None;
            }
        }
    }

    fn take_clip(&self, platform: &dyn Platform) {
        let Ok(mut slot) = self.clip.lock() else {
            return;
        };
        let Some(clip) = slot.as_ref() else {
            return;
        };
        match platform.keep_content(&clip.name, "video/x-msvideo", &clip.bytes) {
            Ok(kept) => {
                if let Ok(mut url) = self.url.lock() {
                    *url = Some(format!("tilo-blob://{}", kept.hash));
                }
            }
            Err(error) => {
                if let Ok(mut error_slot) = self.error.lock() {
                    *error_slot = Some(error);
                }
            }
        }
        *slot = None;
        self.finished.store(true, Ordering::Relaxed);
    }
}

fn jobs() -> &'static Mutex<std::collections::BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<std::collections::BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn next_job_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("camera-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn reads<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| error.to_string())
}

/// The bytes a `tilo-blob://` address holds, read from the device's content store.
fn bytes_of(platform: &dyn Platform, url: &str) -> Result<Vec<u8>, String> {
    let hash = url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{url}` is no content address"))?;
    platform
        .read_content(hash)
        .ok_or_else(|| format!("the content store does not hold `{hash}`"))
}

/// The file a kept picture or clip is, as the runtime reads a picture's answer.
fn file_json(name: &str, media: &str, bytes: usize, hash: &str) -> String {
    serde_json::json!({
        "url": format!("tilo-blob://{hash}"),
        "name": name,
        "media": media,
        "size": bytes,
    })
    .to_string()
}

fn cameras() -> String {
    let devices = tilo_capture::devices(tilo_capture::DeviceKind::Camera)
        .into_iter()
        .map(|device| serde_json::json!({ "id": device.id, "label": device.label }))
        .collect::<Vec<_>>();
    let prompt = tilo_capture::availability(tilo_capture::Feature::Camera)
        == tilo_capture::Availability::Prompt;
    serde_json::json!({ "devices": devices, "prompt": prompt }).to_string()
}

#[derive(Deserialize)]
struct PictureAsk {
    #[serde(default)]
    device: String,
    width: u32,
    height: u32,
}

impl PictureAsk {
    fn request(&self) -> tilo_capture::CameraRequest {
        tilo_capture::CameraRequest {
            device: (!self.device.is_empty()).then(|| self.device.clone()),
            width: self.width.clamp(1, MAX_STILL_EDGE),
            height: self.height.clamp(1, MAX_STILL_EDGE),
        }
    }

    fn key(&self) -> String {
        format!("{}x{}:{}", self.width, self.height, self.device)
    }
}

fn still(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        #[serde(flatten)]
        picture: PictureAsk,
        #[serde(default)]
        format: String,
        #[serde(default)]
        name: String,
    }
    let ask: Ask = reads(input)?;
    let encoding = if ask.format == "png" {
        Encoding::Png
    } else {
        Encoding::Jpeg { quality: 90 }
    };
    picture_with(platform, &ask.picture, None, encoding, ask.name)
}

/// The newest frame the camera shows, kept small: what a viewfinder shows while it asks again.
fn frame(platform: &dyn Platform, input: &str) -> Result<String, String> {
    let ask: PictureAsk = reads(input)?;
    picture_with(
        platform,
        &ask,
        Some(PREVIEW_WIDTH),
        Encoding::Jpeg { quality: 80 },
        String::new(),
    )
}

/// The newest frame the camera shows, kept as the picture `encoding` writes and named `name`
/// (or by the clock when that is empty), scaled to `max_width` at most when the caller caps it.
fn picture_with(
    platform: &dyn Platform,
    ask: &PictureAsk,
    max_width: Option<u32>,
    encoding: Encoding,
    name: String,
) -> Result<String, String> {
    let frame = ask_frame(ask)?;
    let image = match max_width {
        Some(width) => fit(&frame, width),
        None => DecodedImage {
            rgba: frame.rgba.clone(),
            width: frame.width,
            height: frame.height,
        },
    };
    let bytes =
        tilo_image_codec::encode_now(&image, encoding).map_err(|error| error.to_string())?;
    let name = if name.is_empty() {
        let extension = if bytes.starts_with(b"\x89PNG") {
            "png"
        } else {
            "jpg"
        };
        format!("IMG_{}.{extension}", now_name())
    } else {
        name
    };
    let kept = platform.keep_content(&name, encoding.mime(), &bytes)?;
    Ok(file_json(&name, encoding.mime(), bytes.len(), &kept.hash))
}

/// The frame scaled to `width` at most, its aspect kept.
fn fit(frame: &tilo_capture::Frame, width: u32) -> DecodedImage {
    let wanted = width.clamp(1, frame.width.max(1));
    let height = u64::from(frame.height)
        .saturating_mul(u64::from(wanted))
        .checked_div(u64::from(frame.width.max(1)))
        .unwrap_or(0);
    let height = u32::try_from(height.max(1)).unwrap_or_else(|_| frame.height.max(1));
    if wanted == frame.width && height == frame.height {
        return DecodedImage {
            rgba: frame.rgba.clone(),
            width: frame.width,
            height: frame.height,
        };
    }
    let pixels = usize::try_from(wanted)
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX))
        .saturating_mul(4);
    let mut rgba = Vec::with_capacity(pixels);
    for y in 0..height {
        let source_y = u64::from(y)
            .saturating_mul(u64::from(frame.height))
            .checked_div(u64::from(height))
            .unwrap_or(0);
        for x in 0..wanted {
            let source_x = u64::from(x)
                .saturating_mul(u64::from(frame.width))
                .checked_div(u64::from(wanted))
                .unwrap_or(0);
            let at = source_y
                .saturating_mul(u64::from(frame.width))
                .saturating_add(source_x)
                .saturating_mul(4);
            let at = usize::try_from(at).unwrap_or(0);
            match frame.rgba.get(at..at.saturating_add(4)) {
                Some(pixel) => rgba.extend_from_slice(pixel),
                None => rgba.extend_from_slice(&[0, 0, 0, 255]),
            }
        }
    }
    DecodedImage {
        rgba,
        width: wanted,
        height,
    }
}

/// A name from the clock, as the captures the app saves are called.
fn now_name() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    format!("{millis}")
}

fn codes(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        #[serde(default = "default_code_width")]
        width: u32,
    }
    let ask: Ask = reads(input)?;
    let bytes = bytes_of(platform, &ask.url)?;
    let decoded = tilo_image_codec::DecodeImage {
        encoded: &bytes,
        max_dimension: ask.width.clamp(64, 2048),
    }
    .decode_native()
    .map_err(|error| format!("the picture does not decode: {error:?}"))?;
    let frame = tilo_capture::Frame {
        width: decoded.width,
        height: decoded.height,
        rgba: decoded.rgba,
    };
    let luma = tilo_capture::codes::grey(&frame);
    let found = tilo_capture::codes::decode(
        &luma,
        usize::try_from(frame.width).unwrap_or(0),
        usize::try_from(frame.height).unwrap_or(0),
    );
    let (width, height) = (
        f64::from(frame.width.max(1)),
        f64::from(frame.height.max(1)),
    );
    // The box a code covers, as the fraction of the picture the view draws over: from the four
    // corners the detector reported, so the outline sits on the code, not near it.
    let found = found
        .into_iter()
        .take(MAX_CODES)
        .map(|code| {
            let xs = code.corners.map(|(x, _)| f64::from(x));
            let ys = code.corners.map(|(_, y)| f64::from(y));
            let left = xs.iter().copied().fold(f64::INFINITY, f64::min) / width;
            let right = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max) / width;
            let top = ys.iter().copied().fold(f64::INFINITY, f64::min) / height;
            let bottom = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max) / height;
            serde_json::json!({
                "text": code.text,
                "x": left.clamp(0.0, 1.0),
                "y": top.clamp(0.0, 1.0),
                "w": (right - left).clamp(0.0, 1.0),
                "h": (bottom - top).clamp(0.0, 1.0),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "codes": found }).to_string())
}

fn default_code_width() -> u32 {
    1024
}

fn ocr(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        #[serde(default = "default_language")]
        language: String,
    }
    let ask: Ask = reads(input)?;
    let bytes = bytes_of(platform, &ask.url)?;
    let decoded = tilo_image_codec::DecodeImage {
        encoded: &bytes,
        max_dimension: MAX_STILL_EDGE,
    }
    .decode_native()
    .map_err(|error| format!("the page does not decode: {error:?}"))?;
    let recognition =
        tilo_ocr::recognize_detailed(&decoded, &ask.language).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "text": recognition.document.plain_text(),
        "words": recognition.document.words.len(),
        "deskew": recognition.deskew_degrees.unwrap_or(0.0),
    })
    .to_string())
}

fn default_language() -> String {
    "eng".to_owned()
}

fn pages_pdf(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Page {
        url: String,
        #[serde(default)]
        text: String,
    }
    #[derive(Deserialize)]
    struct Ask {
        pages: Vec<Page>,
        #[serde(default = "default_pdf_name")]
        name: String,
    }
    let ask: Ask = reads(input)?;
    let mut pages = Vec::with_capacity(ask.pages.len());
    for page in &ask.pages {
        let bytes = bytes_of(platform, &page.url)?;
        let decoded = tilo_image_codec::DecodeImage {
            encoded: &bytes,
            max_dimension: MAX_STILL_EDGE,
        }
        .decode_native()
        .map_err(|error| format!("the page does not decode: {error:?}"))?;
        let rgb = decoded
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| pixel.iter().take(3).copied())
            .collect();
        pages.push(tilo_doc_pdf::ScannedPdfPage {
            width: decoded.width,
            height: decoded.height,
            rgb,
            recognized_text: page.text.clone(),
        });
    }
    let pdf = tilo_doc_pdf::render_scanned_pdf(&pages);
    let kept = platform.keep_content(&ask.name, "application/pdf", &pdf)?;
    Ok(serde_json::json!({ "url": format!("tilo-blob://{}", kept.hash) }).to_string())
}

fn default_pdf_name() -> String {
    "scanned document.pdf".to_owned()
}

fn scan(_platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        scanner: String,
        #[serde(default = "default_dpi")]
        resolution: u16,
        #[serde(default)]
        colour: String,
        #[serde(default)]
        source: String,
        #[serde(default)]
        name: String,
    }
    let ask: Ask = reads(input)?;
    let settings = tilo_capture::scanner::ScanSettings {
        resolution_dpi: ask.resolution,
        color: match ask.colour.as_str() {
            "grayscale" => tilo_capture::scanner::ScanColor::Grayscale,
            "lineart" => tilo_capture::scanner::ScanColor::LineArt,
            _ => tilo_capture::scanner::ScanColor::Color,
        },
        source: match ask.source.as_str() {
            "feeder" => tilo_capture::scanner::ScanSource::Feeder,
            "feeder-duplex" => tilo_capture::scanner::ScanSource::FeederDuplex,
            _ => tilo_capture::scanner::ScanSource::Flatbed,
        },
    };
    if ask.scanner.is_empty() {
        return Err("no document scanner is chosen".to_owned());
    }
    let id = next_job_id();
    let state = Arc::new(Job::new(false));
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&state));
    let worker = Arc::clone(&state);
    let name = if ask.name.is_empty() {
        format!("scan-{id}.png")
    } else {
        ask.name
    };
    let scanner = ask.scanner;
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    std::thread::Builder::new()
        .name("camera-scan".to_owned())
        .spawn(move || {
            let result = tilo_capture::scanner::scan_page(&scanner, settings);
            let _ = tx.send(result);
            worker.finished.store(true, Ordering::Relaxed);
        })
        .map_err(|error| format!("could not start the scan: {error}"))?;
    if let Ok(mut slot) = state.scan.lock() {
        *slot = Some((name, rx));
    }
    Ok(serde_json::json!({ "job": id }).to_string())
}

fn default_dpi() -> u16 {
    300
}

impl Job {
    fn new(recording: bool) -> Self {
        Self {
            finished: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            recording: AtomicBool::new(recording),
            error: Mutex::new(None),
            url: Mutex::new(None),
            scan: Mutex::new(None),
            clip: Mutex::new(None),
        }
    }
}

/// What a stopped recording answers: its bytes, its duration, and its frame size.
type ClipAnswer = (Vec<u8>, i64, u32, u32);

/// What the camera thread is told, and what it answers.
enum Control {
    /// The newest frame of the camera `key`, opening it when it is not the one open.
    Frame {
        key: String,
        ask: tilo_capture::CameraRequest,
        reply: Sender<Result<tilo_capture::Frame, String>>,
    },
    /// Begin a clip from the frames the camera thread takes.
    StartRecord {
        ask: tilo_capture::CameraRequest,
        reply: Sender<Result<(), String>>,
    },
    /// Finish the clip: its bytes, its duration, and its size.
    StopRecord {
        reply: Sender<Result<ClipAnswer, String>>,
    },
    /// Throw the clip away.
    CancelRecord { reply: Sender<()> },
}

fn keeper() -> Option<&'static Mutex<Sender<Control>>> {
    static KEEPER: OnceLock<Option<Mutex<Sender<Control>>>> = OnceLock::new();
    KEEPER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("camera-keeper".to_owned())
                .spawn(move || keep(&rx))
                .ok()
                .map(|_| Mutex::new(tx))
        })
        .as_ref()
}

/// The camera thread: one capture open at a time, drained for the newest frame, and the clip
/// the app asked for.
fn keep(rx: &Receiver<Control>) {
    let mut open: Option<(String, tilo_capture::VideoCapture)> = None;
    let mut latest: Option<tilo_capture::Frame> = None;
    let mut recorder: Option<media::clip::Recorder> = None;
    loop {
        let control = rx.recv_timeout(POLL);
        if let Some(capture) = open.as_mut().map(|(_, capture)| capture)
            && let Some(frame) = capture.take_latest()
        {
            if let Some(recorder) = recorder.as_mut() {
                recorder.push(&media::clip::Frame {
                    width: frame.width,
                    height: frame.height,
                    rgba: &frame.rgba,
                });
            }
            latest = Some(frame);
        }
        match control {
            Ok(Control::Frame { key, ask, reply }) => {
                if open.as_ref().is_none_or(|(opened, _)| *opened != key) {
                    open = None;
                    latest = None;
                    match tilo_capture::VideoCapture::open_camera(&ask) {
                        Ok(capture) => open = Some((key, capture)),
                        Err(error) => {
                            let _ = reply.send(Err(error.to_string()));
                            continue;
                        }
                    }
                }
                // A frame already taken is the answer; a camera that just opened is waited for
                // its first, for as long as a person waits before calling it broken.
                let waited = std::time::Instant::now();
                while let Some(capture) = open.as_mut().map(|(_, capture)| capture) {
                    if let Some(frame) = capture.take_latest() {
                        latest = Some(frame);
                        break;
                    }
                    if std::time::Instant::now().duration_since(waited) > FIRST_FRAME {
                        break;
                    }
                    std::thread::sleep(POLL);
                }
                let _ = match latest.as_ref() {
                    Some(frame) => reply.send(Ok(frame.clone())),
                    None => reply.send(Err("the camera shows no picture".to_owned())),
                };
            }
            Ok(Control::StartRecord { ask, reply }) => {
                let key = format!(
                    "{}x{}:{}",
                    ask.width,
                    ask.height,
                    ask.device.clone().unwrap_or_default()
                );
                if open.as_ref().is_none_or(|(opened, _)| *opened != key) {
                    open = None;
                    latest = None;
                    match tilo_capture::VideoCapture::open_camera(&ask) {
                        Ok(capture) => open = Some((key, capture)),
                        Err(error) => {
                            let _ = reply.send(Err(error.to_string()));
                            continue;
                        }
                    }
                }
                recorder = Some(media::clip::Recorder::new(
                    media::clip::FPS,
                    encode_jpeg_rgb,
                ));
                let _ = reply.send(Ok(()));
            }
            Ok(Control::StopRecord { reply }) => match recorder.take() {
                Some(clip) => {
                    let (w, h) = clip.dimensions();
                    clip.finish(Box::new(move |result| {
                        let _ = reply.send(match result {
                            Ok((bytes, duration_ms)) => Ok((bytes, duration_ms, w, h)),
                            Err(error) => Err(error),
                        });
                    }));
                }
                None => {
                    let _ = reply.send(Err("nothing recorded".to_owned()));
                }
            },
            Ok(Control::CancelRecord { reply }) => {
                recorder = None;
                let _ = reply.send(());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Ask the camera thread for a frame.
fn ask_frame(ask: &PictureAsk) -> Result<tilo_capture::Frame, String> {
    let handle = keeper().ok_or_else(|| "the camera thread could not start".to_owned())?;
    let (reply, answered) = std::sync::mpsc::channel();
    handle
        .lock()
        .map_err(|_| "the camera thread is unreadable".to_owned())?
        .send(Control::Frame {
            key: ask.key(),
            ask: ask.request(),
            reply,
        })
        .map_err(|_| "the camera thread is gone".to_owned())?;
    answered
        .recv_timeout(FIRST_FRAME.saturating_add(POLL))
        .map_err(|_| "the camera did not answer".to_owned())?
}

fn record(input: &str) -> Result<String, String> {
    let ask: PictureAsk = reads(input)?;
    let handle = keeper().ok_or_else(|| "the camera thread could not start".to_owned())?;
    let (reply, answered) = std::sync::mpsc::channel();
    handle
        .lock()
        .map_err(|_| "the camera thread is unreadable".to_owned())?
        .send(Control::StartRecord {
            ask: ask.request(),
            reply,
        })
        .map_err(|_| "the camera thread is gone".to_owned())?;
    answered
        .recv_timeout(FIRST_FRAME.saturating_add(POLL))
        .map_err(|_| "the camera did not answer".to_owned())??;
    let id = next_job_id();
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::new(Job::new(true)));
    Ok(serde_json::json!({ "job": id }).to_string())
}

fn stop_record(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        job: String,
    }
    let ask: Ask = reads(input)?;
    let state = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .get(&ask.job)
        .cloned()
        .ok_or_else(|| format!("no job `{}` runs on this device", ask.job))?;
    let handle = keeper().ok_or_else(|| "the camera thread could not start".to_owned())?;
    let (reply, answered) = std::sync::mpsc::channel();
    handle
        .lock()
        .map_err(|_| "the camera thread is unreadable".to_owned())?
        .send(Control::StopRecord { reply })
        .map_err(|_| "the camera thread is gone".to_owned())?;
    let (bytes, duration_ms, w, h) = answered
        .recv_timeout(std::time::Duration::from_secs(60))
        .map_err(|_| "the clip was not finished".to_owned())??;
    let name = format!("VID_{}.avi", now_name());
    if let Ok(mut slot) = state.clip.lock() {
        *slot = Some(Clip {
            name: name.clone(),
            bytes: bytes.clone(),
        });
    }
    Ok(serde_json::json!({
        "video": {
            "name": name,
            "duration_ms": duration_ms,
            "w": w,
            "h": h,
            "size": bytes.len(),
        }
    })
    .to_string())
}

/// The JPEG encoder the clip recorder feeds, through the one image codec.
fn encode_jpeg_rgb(rgb: &[u8], width: u32, height: u32, quality: u8) -> Option<Vec<u8>> {
    let mut rgba = Vec::with_capacity((rgb.len() / 3).saturating_mul(4));
    for pixel in rgb.chunks_exact(3) {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    tilo_image_codec::encode_now(
        &DecodedImage {
            rgba,
            width,
            height,
        },
        Encoding::Jpeg { quality },
    )
    .ok()
}
