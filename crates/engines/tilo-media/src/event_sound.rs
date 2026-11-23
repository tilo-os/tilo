//! Session-owned desktop event-sound delivery.
//!
//! The compositor submits a cue to a bounded worker and never forks or waits on the
//! render thread; the freedesktop sound-theme lookup happens here and `pw-play` plays
//! the file on the session route. Missing audio support is reported as a capability,
//! never faked.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundBackend {
    PipeWire,
    #[cfg(windows)]
    WindowsSystem,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoundCue {
    pub ids: &'static [&'static str],
    pub description: &'static str,
    pub theme: String,
    pub volume: f32,
}

#[cfg(unix)]
pub fn sound_backend() -> SoundBackend {
    if command_exists("pw-play") && !installed_themes().is_empty() {
        SoundBackend::PipeWire
    } else {
        SoundBackend::Unavailable
    }
}

#[cfg(windows)]
pub fn sound_backend() -> SoundBackend {
    SoundBackend::WindowsSystem
}

#[cfg(not(any(unix, windows)))]
pub fn sound_backend() -> SoundBackend {
    SoundBackend::Unavailable
}

#[cfg(unix)]
fn command_exists(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            std::fs::metadata(dir.join(name)).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
    })
}

fn sound_roots() -> Vec<PathBuf> {
    tilo_paths::xdg::data_home()
        .into_iter()
        .chain(tilo_paths::xdg::data_dirs())
        .map(|root| root.join("sounds"))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundTheme {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ThemeIndex {
    name: Option<String>,
    directories: Vec<String>,
    inherits: Vec<String>,
}

fn parse_index_theme(text: &str) -> Option<ThemeIndex> {
    let mut in_theme = false;
    let mut seen = false;
    let mut index = ThemeIndex::default();
    let list = |value: &str| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| is_path_component(item))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_theme = line == "[Sound Theme]";
            seen |= in_theme;
            continue;
        }
        if !in_theme {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => index.name = Some(value.trim().to_string()),
            "Directories" => index.directories = list(value),
            "Inherits" => index.inherits = list(value),
            _ => {}
        }
    }
    seen.then_some(index)
}

fn is_path_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

fn theme_index(roots: &[PathBuf], theme: &str) -> Option<(PathBuf, ThemeIndex)> {
    if !is_path_component(theme) {
        return None;
    }
    roots.iter().find_map(|root| {
        let dir = root.join(theme);
        let text = std::fs::read_to_string(dir.join("index.theme")).ok()?;
        Some((dir, parse_index_theme(&text)?))
    })
}

pub fn installed_themes() -> Vec<SoundTheme> {
    installed_themes_in(&sound_roots())
}

fn installed_themes_in(roots: &[PathBuf]) -> Vec<SoundTheme> {
    let mut themes: Vec<SoundTheme> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if themes.iter().any(|theme| theme.id == id) {
                continue;
            }
            let Some((_, index)) = theme_index(std::slice::from_ref(root), &id) else {
                continue;
            };
            let name = index
                .name
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| id.clone());
            themes.push(SoundTheme { id, name });
        }
    }
    themes.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    themes
}

pub fn resolve_sound_file(theme: &str, ids: &[&str]) -> Option<PathBuf> {
    resolve_sound_file_in(&sound_roots(), theme, ids)
}

fn resolve_sound_file_in(roots: &[PathBuf], theme: &str, ids: &[&str]) -> Option<PathBuf> {
    // Breadth-first over the inheritance chain, bounded so a cycle between
    // two themes cannot spin forever.
    let mut chain = vec![theme.to_string()];
    let mut next = 0_usize;
    while let Some(name) = chain.get(next).cloned()
        && chain.len() <= 16
    {
        next = next.saturating_add(1);
        if let Some((_, index)) = theme_index(roots, &name) {
            for parent in index.inherits {
                if !chain.contains(&parent) {
                    chain.push(parent);
                }
            }
        }
    }
    if !chain.iter().any(|name| name == "freedesktop") {
        chain.push("freedesktop".into());
    }
    for id in ids.iter().filter(|id| is_path_component(id)) {
        for name in &chain {
            let Some((dir, index)) = theme_index(roots, name) else {
                continue;
            };
            for sub in &index.directories {
                for ext in ["oga", "ogg", "wav"] {
                    let path = dir.join(sub).join(format!("{id}.{ext}"));
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

fn play_one(backend: SoundBackend, cue: &SoundCue) {
    if cue.volume <= 0.0 {
        return;
    }
    let command = match backend {
        #[cfg(windows)]
        SoundBackend::WindowsSystem => {
            #[link(name = "user32")]
            unsafe extern "system" {
                fn MessageBeep(sound_type: u32) -> i32;
            }
            // MessageBeep takes no pointers and asynchronously plays the
            // user's configured system sound. It never displays a dialog.
            if unsafe { MessageBeep(0) } == 0 {
                tracing::debug!("media: Windows system sound unavailable");
            }
            return;
        }
        SoundBackend::PipeWire => {
            let Some(path) = resolve_sound_file(&cue.theme, cue.ids) else {
                return;
            };
            let mut c = Command::new("pw-play");
            c.arg(format!("--volume={:.3}", cue.volume.clamp(0.0, 1.0)));
            c.arg(path);
            c
        }
        SoundBackend::Unavailable => return,
    };
    #[cfg(not(target_arch = "wasm32"))]
    let _ = bounded_command::run_prepared(command, None, 1024, std::time::Duration::from_secs(5));
    #[cfg(target_arch = "wasm32")]
    drop(command);
}

#[derive(Debug)]
struct Inner {
    sender: Mutex<Option<mpsc::SyncSender<SoundCue>>>,
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

#[derive(Clone, Debug)]
pub struct EventSoundPlayer(Arc<Inner>);

impl EventSoundPlayer {
    pub fn new() -> Self {
        let backend = sound_backend();
        let (sender, receiver) = mpsc::sync_channel::<SoundCue>(1);
        let worker = std::thread::Builder::new()
            .name("tilo-event-sound".into())
            .spawn(move || {
                while let Ok(cue) = receiver.recv() {
                    play_one(backend, &cue);
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

    /// Queue `cue`. `false` when there is no backend or a cue is already
    /// waiting.
    pub fn play(&self, cue: SoundCue) -> bool {
        self.available()
            && self
                .0
                .sender
                .lock()
                .ok()
                .and_then(|sender| sender.as_ref().map(|sender| sender.try_send(cue).is_ok()))
                .unwrap_or(false)
    }
}

impl Default for EventSoundPlayer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cue() -> SoundCue {
        SoundCue {
            ids: &["message-new-instant", "message"],
            description: "New message",
            theme: "ocean".into(),
            volume: 0.5,
        }
    }

    #[test]
    fn unavailable_backend_never_claims_delivery() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let player = EventSoundPlayer(Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(None),
            backend: SoundBackend::Unavailable,
        }));
        assert!(!player.available());
        assert!(!player.play(cue()));
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn themes_resolve_through_inheritance_and_fall_back_to_freedesktop() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sounds");
        write(
            &root.join("freedesktop/index.theme"),
            "[Sound Theme]\nName=Default\nDirectories=stereo\n",
        );
        write(&root.join("freedesktop/stereo/message.oga"), "");
        write(&root.join("freedesktop/stereo/device-added.oga"), "");
        write(
            &root.join("ocean/index.theme"),
            "[Sound Theme]\nName=Ocean\nInherits=beach\nDirectories=stereo\n",
        );
        write(&root.join("ocean/stereo/message-new-instant.ogg"), "");
        write(
            &root.join("beach/index.theme"),
            "[Sound Theme]\nName=Beach\nInherits=ocean\nDirectories=stereo\n",
        );
        write(&root.join("beach/stereo/device-added.wav"), "");
        write(&root.join("not-a-theme/stereo/x.oga"), "");
        let roots = vec![root.clone()];

        let themes: Vec<_> = installed_themes_in(&roots)
            .into_iter()
            .map(|theme| (theme.id, theme.name))
            .collect();
        assert_eq!(
            themes,
            [
                ("beach".to_string(), "Beach".to_string()),
                ("freedesktop".into(), "Default".into()),
                ("ocean".into(), "Ocean".into()),
            ]
        );
        assert_eq!(
            resolve_sound_file_in(&roots, "ocean", &["message-new-instant", "message"]),
            Some(root.join("ocean/stereo/message-new-instant.ogg"))
        );
        assert_eq!(
            resolve_sound_file_in(&roots, "ocean", &["device-added"]),
            Some(root.join("beach/stereo/device-added.wav")),
            "inherited, and the ocean ↔ beach cycle terminates"
        );
        assert_eq!(
            resolve_sound_file_in(&roots, "beach", &["message"]),
            Some(root.join("freedesktop/stereo/message.oga"))
        );
        assert_eq!(
            resolve_sound_file_in(&roots, "../freedesktop", &["message"]),
            Some(root.join("freedesktop/stereo/message.oga")),
            "a hostile theme name only reaches the freedesktop fallback"
        );
        assert_eq!(resolve_sound_file_in(&roots, "ocean", &["../../x"]), None);
    }
}
