//! The screenshots the portal asks for, taken from the compositor's
//! frames.
//!
//! The portal runs on the desktop services' runtime and the compositor on
//! the session's loop, so the portal's side ([`ScreenCapture`]) only sends
//! a request and wakes the loop; the session answers it on its next pass
//! ([`Captures::serve`]) with a PNG of every display as they show.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_compositor::Compositor;
use tilo_portal::{Capture, Pending};
use tilo_service::Wake;

type Reply = tokio::sync::oneshot::Sender<Result<PathBuf, String>>;

/// The portal's side: asks the session for a screenshot.
pub struct ScreenCapture {
    requests: Sender<Reply>,
    wake: Wake,
}

/// The session's side: the screenshots asked for, and where they go.
pub struct Captures {
    requests: Receiver<Reply>,
    directory: PathBuf,
}

/// A portal side and the session side that answers it, the screenshots
/// written under `directory`; `wake` wakes the session's loop.
#[must_use]
pub fn channel_to(directory: PathBuf, wake: Wake) -> (ScreenCapture, Captures) {
    let (requests, asked) = channel();
    (
        ScreenCapture { requests, wake },
        Captures {
            requests: asked,
            directory,
        },
    )
}

/// Where the session writes the screenshots apps asked for: under the
/// runtime directory, which only this user reads.
#[must_use]
pub fn directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("tilo-screenshots")
}

impl Capture for ScreenCapture {
    fn screenshot(&self) -> Pending<Result<PathBuf, String>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        if self.requests.send(reply).is_err() {
            return Box::pin(async { Err("the session ended".to_owned()) });
        }
        (self.wake)();
        Box::pin(async move {
            answer
                .await
                .unwrap_or_else(|_| Err("the session ended".to_owned()))
        })
    }
}

impl Captures {
    /// Answer every screenshot asked for since the last pass.
    pub fn serve(&self, compositor: &mut Compositor) {
        while let Ok(reply) = self.requests.try_recv() {
            let _ = reply.send(self.take(compositor));
        }
    }

    fn take(&self, compositor: &mut Compositor) -> Result<PathBuf, String> {
        let picture = picture(compositor)?;
        std::fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let path = unique(&self.directory);
        picture
            .save_with_format(&path, image::ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        Ok(path)
    }
}

/// A fresh file name under `directory`.
fn unique(directory: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    directory.join(format!("screenshot-{stamp}.png"))
}

/// Every display on, each at its place in the layout.
pub(crate) fn picture(compositor: &mut Compositor) -> Result<image::RgbaImage, String> {
    let mut placed = Vec::new();
    for output in compositor.outputs() {
        let Some((x, y, width, _)) = output.rect else {
            continue;
        };
        let shot = compositor
            .read_back(&tilo_compositor::capture::Target::output(
                output.name.clone(),
            ))
            .map_err(|error| error.to_string())?;
        // The frame is in the output's pixels: its place in the layout
        // scales with it.
        let scale = f64::from(shot.width) / f64::from(width.max(1));
        let Some(frame) = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba) else {
            return Err(format!("{}: a frame of the wrong size", output.name));
        };
        placed.push((scaled(x, scale), scaled(y, scale), frame));
    }
    let left = placed
        .iter()
        .map(|(x, _, _)| *x)
        .min()
        .ok_or("no display is on")?;
    let top = placed.iter().map(|(_, y, _)| *y).min().unwrap_or(0);
    let right = placed
        .iter()
        .map(|(x, _, frame)| x.saturating_add(i64::from(frame.width())))
        .max()
        .unwrap_or(left);
    let bottom = placed
        .iter()
        .map(|(_, y, frame)| y.saturating_add(i64::from(frame.height())))
        .max()
        .unwrap_or(top);
    let size = |from: i64, to: i64| u32::try_from(to.saturating_sub(from)).unwrap_or(0);
    let mut picture = image::RgbaImage::new(size(left, right), size(top, bottom));
    for (x, y, frame) in placed {
        image::imageops::replace(
            &mut picture,
            &frame,
            x.saturating_sub(left),
            y.saturating_sub(top),
        );
    }
    Ok(picture)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a layout coordinate in pixels is far from i64's limits"
)]
fn scaled(at: i32, scale: f64) -> i64 {
    (f64::from(at) * scale).round() as i64
}
