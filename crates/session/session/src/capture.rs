//! The screenshots the portal asks for, taken from the compositor's
//! frames.
//!
//! The portal runs on the desktop services' runtime and the compositor on
//! the session's loop, so the portal's side ([`ScreenCapture`]) only sends
//! a request and wakes the loop; the session answers it on its next pass
//! ([`Captures::serve`]) with a PNG of what was asked for as it shows.
//!
//! What only the person can say — which window, which area — is asked in
//! the capture studio's selector ([`crate::capture_studio`]), so the answer
//! comes a pass after they took it, however long that was. A request the
//! app stops waiting for takes the selector down with it: the future it
//! awaited says so as it drops.
//!
//! A colour pick is answered on that same pass, from the same picture: the
//! pixel under the pointer where the screenshot of every display puts it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_compositor::Compositor;
use tilo_portal::{Capture, CaptureError, CaptureTarget, Pending};
use tilo_service::Wake;

use crate::capture_studio::CaptureStudio;

/// What the portal asks for: what to capture, where its answer goes, and
/// the request it belongs to.
enum Asked {
    Screenshot {
        id: u64,
        target: CaptureTarget,
        reply: Reply,
    },
    /// The colour under the pointer, as red, green and blue from 0 to 1.
    Color { reply: ColorReply },
    /// Nobody waits for request `id` any more: whatever is up for it comes
    /// down. An ask already answered is no longer up, so this changes
    /// nothing for it.
    Withdrawn { id: u64 },
}

/// The answer: the file, or why there is none.
type Reply = tokio::sync::oneshot::Sender<Result<PathBuf, CaptureError>>;

/// The answer to a pick: the colour, or why there is none.
type ColorReply = tokio::sync::oneshot::Sender<Result<(f64, f64, f64), CaptureError>>;

/// The portal's side: asks the session for a screenshot.
pub struct ScreenCapture {
    requests: Sender<Asked>,
    wake: Wake,
    /// The next request's number.
    next: AtomicU64,
    /// Whether the session serves the selector the person picks a window or
    /// draws an area in, which it says on its first pass with one.
    selector: Arc<AtomicBool>,
}

/// The session's side: the screenshots asked for, and where they go.
pub struct Captures {
    requests: Receiver<Asked>,
    directory: PathBuf,
    selector: Arc<AtomicBool>,
}

/// A portal side and the session side that answers it, the screenshots
/// written under `directory`; `wake` wakes the session's loop.
#[must_use]
pub fn channel_to(directory: PathBuf, wake: Wake) -> (ScreenCapture, Captures) {
    let (requests, asked) = channel();
    let selector = Arc::new(AtomicBool::new(false));
    (
        ScreenCapture {
            requests,
            wake,
            next: AtomicU64::new(1),
            selector: Arc::clone(&selector),
        },
        Captures {
            requests: asked,
            directory,
            selector,
        },
    )
}

/// Where the session writes the screenshots apps asked for: under the
/// runtime directory, which only this user reads. Never the person's own
/// folders: the app is handed its own path into the store instead.
#[must_use]
pub fn directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("tilo-screenshots")
}

/// One request as the portal side holds it while it waits: dropped, it tells
/// the session nobody wants the picture any more, which takes down whatever
/// was up for it. An ask the session already answered is no longer held, so
/// that notice changes nothing there.
struct Asking {
    id: u64,
    requests: Sender<Asked>,
    wake: Wake,
}

impl Drop for Asking {
    fn drop(&mut self) {
        if self.requests.send(Asked::Withdrawn { id: self.id }).is_ok() {
            (self.wake)();
        }
    }
}

impl Capture for ScreenCapture {
    fn screenshot(&self, target: CaptureTarget) -> Pending<Result<PathBuf, CaptureError>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = tokio::sync::oneshot::channel();
        let asking = Asking {
            id,
            requests: self.requests.clone(),
            wake: self.wake.clone(),
        };
        if self
            .requests
            .send(Asked::Screenshot { id, target, reply })
            .is_err()
        {
            return Box::pin(async { Err(CaptureError::Failed("the session ended".to_owned())) });
        }
        (self.wake)();
        Box::pin(async move {
            // Held until this future drops, so a request the app stops
            // waiting for takes the selection down with it.
            let _asking = asking;
            answer
                .await
                .unwrap_or_else(|_| Err(CaptureError::Failed("the session ended".to_owned())))
        })
    }

    fn color_at_pointer(&self) -> Pending<Result<(f64, f64, f64), CaptureError>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        if self.requests.send(Asked::Color { reply }).is_err() {
            return Box::pin(async { Err(CaptureError::Failed("the session ended".to_owned())) });
        }
        (self.wake)();
        Box::pin(async move {
            answer
                .await
                .unwrap_or_else(|_| Err(CaptureError::Failed("the session ended".to_owned())))
        })
    }

    /// What this session takes: everything the displays show and the window
    /// with focus, and — while it serves the studio's selector — the window
    /// the person picks and the area they draw.
    fn targets(&self) -> u32 {
        let mut bits = CaptureTarget::Screen.bit() | CaptureTarget::ActiveWindow.bit();
        if self.selector.load(Ordering::Acquire) {
            bits |= CaptureTarget::Window.bit() | CaptureTarget::Area.bit();
        }
        bits
    }
}

impl Captures {
    /// Answer every screenshot and pick asked for since the last pass: what
    /// the session captures itself, and what the person takes in `studio`'s
    /// selector.
    pub(crate) fn serve(&self, compositor: &mut Compositor, studio: Option<&mut CaptureStudio>) {
        let mut studio = studio;
        if studio.is_some() {
            // From here on there is a selector to ask the person in, so
            // apps may ask for what only they can name.
            self.selector.store(true, Ordering::Release);
        }
        while let Ok(asked) = self.requests.try_recv() {
            match asked {
                Asked::Screenshot { id, target, reply } => match target {
                    CaptureTarget::Screen | CaptureTarget::ActiveWindow => {
                        let _ = reply.send(self.take(compositor, target));
                    }
                    CaptureTarget::Window | CaptureTarget::Area => {
                        let Some(selector) = studio.as_mut() else {
                            let _ = reply.send(Err(CaptureError::Failed(format!(
                                "no selector is drawn to ask for a {target:?}"
                            ))));
                            continue;
                        };
                        selector.ask_selection(target, id, reply, self.directory.clone());
                    }
                },
                Asked::Color { reply } => {
                    let _ = reply.send(colour(compositor));
                }
                Asked::Withdrawn { id } => {
                    if let Some(selector) = studio.as_mut() {
                        selector.withdraw(id);
                    }
                }
            }
        }
    }

    fn take(
        &self,
        compositor: &mut Compositor,
        target: CaptureTarget,
    ) -> Result<PathBuf, CaptureError> {
        let picture = match target {
            CaptureTarget::Screen => picture(compositor),
            CaptureTarget::ActiveWindow => focused(compositor),
            CaptureTarget::Window | CaptureTarget::Area => {
                return Err(CaptureError::Failed(format!(
                    "a {target:?} is taken by the person, not here"
                )));
            }
        }
        .map_err(CaptureError::Failed)?;
        keep(&self.directory, &picture).map_err(CaptureError::Failed)
    }
}

/// Write `picture` as a PNG under `directory`, named fresh; its path.
pub(crate) fn keep(directory: &Path, picture: &image::RgbaImage) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = unique(directory, "screenshot", "png");
    picture
        .save_with_format(&path, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(path)
}

/// The window with the keyboard's focus, as the compositor draws it.
fn focused(compositor: &mut Compositor) -> Result<image::RgbaImage, String> {
    let identifier = compositor
        .focused_shareable_window()
        .ok_or("no window has focus")?;
    let shot = compositor
        .read_back(&tilo_compositor::capture::Target::window(identifier))
        .map_err(|error| error.to_string())?;
    image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba)
        .ok_or_else(|| "a frame of the wrong size".to_owned())
}

/// A fresh file name under `directory`, `kind` and its extension: what the
/// person keeps and what an app is handed are named the same way.
pub(crate) fn unique(directory: &Path, kind: &str, extension: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    directory.join(format!("{kind}-{stamp}.{extension}"))
}

/// Every display on, each at its place in the layout.
pub(crate) fn picture(compositor: &mut Compositor) -> Result<image::RgbaImage, String> {
    Ok(desk(compositor)?.picture)
}

/// The colour under the pointer: the pixel under it where a screenshot of
/// every display puts it, as red, green and blue from 0 to 1.
fn colour(compositor: &mut Compositor) -> Result<(f64, f64, f64), CaptureError> {
    let pointer = compositor.pointer_location();
    let none = || CaptureError::Failed(tilo_portal::NO_COLOUR_UNDER_POINTER.to_owned());
    // Nothing under the pointer is nothing to pick, whether no display is
    // on or the pointer is over none of them.
    if !compositor
        .outputs()
        .iter()
        .filter_map(|output| output.rect)
        .any(|rect| holds(rect, pointer))
    {
        return Err(none());
    }
    let desk = desk(compositor).map_err(CaptureError::Failed)?;
    desk.colour_at(pointer).ok_or_else(none)
}

/// A picture of every display on, each at its place in the layout, and
/// where each one landed in it.
struct Desk {
    picture: image::RgbaImage,
    placed: Vec<Placed>,
}

/// Where one display landed in a [`Desk`].
struct Placed {
    /// The display's rectangle in the layout.
    rect: (i32, i32, i32, i32),
    /// Where the display's top-left pixel is in the picture.
    at: (i64, i64),
    /// How many picture pixels stand for one point of the layout.
    scale: f64,
}

/// One display read back, before it is placed in a [`Desk`].
struct Frame {
    rect: (i32, i32, i32, i32),
    at: (i64, i64),
    scale: f64,
    pixels: image::RgbaImage,
}

fn desk(compositor: &mut Compositor) -> Result<Desk, String> {
    let mut frames = Vec::new();
    for output in compositor.outputs() {
        let Some(rect) = output.rect else {
            continue;
        };
        let shot = compositor
            .read_back(&tilo_compositor::capture::Target::output(
                output.name.clone(),
            ))
            .map_err(|error| error.to_string())?;
        // The frame is in the output's pixels: its place in the layout
        // scales with it.
        let scale = f64::from(shot.width) / f64::from(rect.2.max(1));
        let Some(pixels) = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba) else {
            return Err(format!("{}: a frame of the wrong size", output.name));
        };
        frames.push(Frame {
            rect,
            at: (scaled(rect.0, scale), scaled(rect.1, scale)),
            scale,
            pixels,
        });
    }
    let left = frames
        .iter()
        .map(|frame| frame.at.0)
        .min()
        .ok_or("no display is on")?;
    let top = frames.iter().map(|frame| frame.at.1).min().unwrap_or(0);
    let right = frames
        .iter()
        .map(|frame| frame.at.0.saturating_add(i64::from(frame.pixels.width())))
        .max()
        .unwrap_or(left);
    let bottom = frames
        .iter()
        .map(|frame| frame.at.1.saturating_add(i64::from(frame.pixels.height())))
        .max()
        .unwrap_or(top);
    let size = |from: i64, to: i64| u32::try_from(to.saturating_sub(from)).unwrap_or(0);
    let mut composed = image::RgbaImage::new(size(left, right), size(top, bottom));
    let mut placed = Vec::new();
    for frame in frames {
        let at = (
            frame.at.0.saturating_sub(left),
            frame.at.1.saturating_sub(top),
        );
        image::imageops::replace(&mut composed, &frame.pixels, at.0, at.1);
        placed.push(Placed {
            rect: frame.rect,
            at,
            scale: frame.scale,
        });
    }
    Ok(Desk {
        picture: composed,
        placed,
    })
}

impl Desk {
    /// The colour `pointer`, a point of the layout, is over: red, green and
    /// blue from 0 to 1, as the picture shows them. `None` when the pointer
    /// is over no display.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a pixel inside the picture is far from u32's limits"
    )]
    fn colour_at(&self, pointer: (f64, f64)) -> Option<(f64, f64, f64)> {
        let placed = self
            .placed
            .iter()
            .find(|placed| holds(placed.rect, pointer))?;
        let pixel = |at: i64, from: i32, to: f64, edge: u32| {
            let offset = ((to - f64::from(from)) * placed.scale).floor() as i64;
            let x = at
                .saturating_add(offset)
                .clamp(0, i64::from(edge.saturating_sub(1)));
            u32::try_from(x).ok()
        };
        let x = pixel(placed.at.0, placed.rect.0, pointer.0, self.picture.width())?;
        let y = pixel(placed.at.1, placed.rect.1, pointer.1, self.picture.height())?;
        // The picture's alpha says nothing about the colour: a window that
        // draws nothing shows the desk through, as it does on the display.
        let [red, green, blue, _] = self.picture.get_pixel(x, y).0;
        Some((
            f64::from(red) / 255.0,
            f64::from(green) / 255.0,
            f64::from(blue) / 255.0,
        ))
    }
}

/// Whether `point`, a point of the layout, is on the display with
/// rectangle `rect`.
fn holds(rect: (i32, i32, i32, i32), point: (f64, f64)) -> bool {
    let (x, y, width, height) = rect;
    point.0 >= f64::from(x)
        && point.0 < f64::from(x.saturating_add(width))
        && point.1 >= f64::from(y)
        && point.1 < f64::from(y.saturating_add(height))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a layout coordinate in pixels is far from i64's limits"
)]
fn scaled(at: i32, scale: f64) -> i64 {
    (f64::from(at) * scale).round() as i64
}
