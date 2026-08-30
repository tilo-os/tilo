//! Session-owned desktop event-sound delivery.
//!
//! The compositor submits a cue to a bounded worker; it never forks or waits
//! on the render thread. libcanberra is preferred because it implements the
//! freedesktop sound-theme lookup and routes through the current PipeWire/
//! Pulse session. A standard theme file plus `pw-play` is the compatible
//! fallback. Missing audio support is reported as a capability, never faked.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundBackend {
    Libcanberra,
    PipeWire,
    Unavailable,
}

pub fn sound_backend() -> SoundBackend {
    if command_exists("canberra-gtk-play") {
        SoundBackend::Libcanberra
    } else if command_exists("pw-play") && themed_sound().is_some() {
        SoundBackend::PipeWire
    } else {
        SoundBackend::Unavailable
    }
}

fn command_exists(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let path = dir.join(name);
            std::fs::metadata(path).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
    })
}

fn themed_sound() -> Option<PathBuf> {
    [
        "/usr/share/sounds/freedesktop/stereo/message-new-instant.oga",
        "/usr/share/sounds/freedesktop/stereo/message.oga",
        "/usr/share/sounds/freedesktop/stereo/complete.oga",
    ]
    .into_iter()
    .map(Path::new)
    .find(|path| path.is_file())
    .map(Path::to_path_buf)
}

fn play_one(backend: SoundBackend) {
    let mut command = match backend {
        SoundBackend::Libcanberra => {
            let mut c = Command::new("canberra-gtk-play");
            c.args(["--id=message-new-instant", "--description=qOS notification"]);
            c
        }
        SoundBackend::PipeWire => {
            let Some(path) = themed_sound() else { return };
            let mut c = Command::new("pw-play");
            c.arg(path);
            c
        }
        SoundBackend::Unavailable => return,
    };
    // One cue at a time. The bounded sender coalesces bursts while the worker
    // waits, preventing notification storms from becoming a process storm.
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // A broken audio route must not strand the session worker or make final
    // compositor shutdown wait forever. Five seconds is far beyond a desktop
    // cue, while still bounding a wedged helper.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
}

#[derive(Debug)]
struct Inner {
    sender: Mutex<Option<mpsc::SyncSender<()>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    backend: SoundBackend,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.sender.get_mut().ok().and_then(Option::take);
        if let Some(worker) = self.worker.get_mut().ok().and_then(Option::take) {
            let _ = worker.join();
        }
    }
}

/// Cloneable session owner. Dropping the final clone closes and joins its
/// worker. `play()` is non-blocking and coalesces bursts at one pending cue.
#[derive(Clone, Debug)]
pub struct NotificationSoundPlayer(Arc<Inner>);

impl NotificationSoundPlayer {
    pub fn new() -> Self {
        let backend = sound_backend();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("qos-notification-sound".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    play_one(backend);
                }
            })
            .ok();
        Self(Arc::new(Inner {
            sender: Mutex::new(worker.as_ref().map(|_| sender)),
            worker: Mutex::new(worker),
            backend,
        }))
    }

    pub fn backend(&self) -> SoundBackend {
        self.0.backend
    }

    pub fn available(&self) -> bool {
        self.backend() != SoundBackend::Unavailable
    }

    pub fn play(&self) -> bool {
        self.0
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(|sender| sender.try_send(()).is_ok()))
            .unwrap_or(false)
    }
}

impl Default for NotificationSoundPlayer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_backend_never_claims_delivery() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let player = NotificationSoundPlayer(Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(None),
            backend: SoundBackend::Unavailable,
        }));
        assert!(!player.available());
    }
}
