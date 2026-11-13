//! Videos opened by their contents where they have no path, kept in the app's cache so
//! Recent can reopen them after a reload. A kept video is named by a `tilo-kept:` source;
//! at most [`KEPT_VIDEOS`] are kept, each at most [`KEEP_LIMIT`] bytes.

use std::sync::mpsc::{Receiver, sync_channel};

use tilo_files::{Dir, Location, VPath};

pub(crate) const KEEP_LIMIT: usize = 512 * 1024 * 1024;
pub(crate) const KEPT_VIDEOS: usize = 3;

const SCHEME: &str = "tilo-kept:";
const APP: &str = "so.tilo.video-player";

/// Whether chosen files have native paths here; without them a video is opened by its
/// contents and kept for Recent.
pub(crate) fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

fn location() -> Option<Location> {
    Some(
        tilo_paths::AppPaths::for_app(APP)
            .cache()
            .join(&VPath::new("kept").ok()?),
    )
}

pub(crate) fn source(token: &str) -> media::Source {
    media::Source::Remote {
        url: format!("{SCHEME}{token}"),
    }
}

pub(crate) fn token_of(source: &media::Source) -> Option<&str> {
    match source {
        media::Source::Remote { url } => url
            .strip_prefix(SCHEME)
            .filter(|token| VPath::new(token).is_ok()),
        _ => None,
    }
}

pub(crate) fn page_local(source: &media::Source) -> bool {
    matches!(source, media::Source::Remote { url } if url.starts_with("blob:"))
}

/// A name for a new copy of `name`, unique enough among the few kept at once.
pub(crate) fn token(name: &str, len: usize) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let stamp = tilo_clock::unix_now_ms();
    for byte in name
        .bytes()
        .chain(len.to_le_bytes())
        .chain(stamp.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Keep `bytes` as `token` and delete every copy not in `retain`; the result says whether
/// it was written.
pub(crate) fn keep(token: String, bytes: Vec<u8>, retain: Vec<String>) -> Receiver<bool> {
    let (tx, rx) = sync_channel(1);
    let Some(location) = location() else {
        let _ = tx.send(false);
        return rx;
    };
    drop(ui::rt::app().spawn_pinned(move || async move {
        let written = match (Dir::open(location), VPath::new(&token)) {
            (Ok(dir), Ok(path)) => {
                let written = dir.write_atomic(&path, &bytes).await.is_ok();
                drop(bytes);
                if let Ok(entries) = dir.list(&VPath::root()).await {
                    for entry in entries {
                        if entry.name != token
                            && !retain.contains(&entry.name)
                            && let Ok(stale) = VPath::new(&entry.name)
                        {
                            let _ = dir.remove(&stale).await;
                        }
                    }
                }
                written
            }
            _ => false,
        };
        let _ = tx.send(written);
    }));
    rx
}

/// The bytes kept as `token`, or `None` once they are gone. `wake` runs when they have
/// been read.
pub(crate) fn recall(token: String, wake: impl Fn() + Send + 'static) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = sync_channel(1);
    let Some(location) = location() else {
        let _ = tx.send(None);
        return rx;
    };
    drop(ui::rt::app().spawn_pinned(move || async move {
        let bytes = match (Dir::open(location), VPath::new(&token)) {
            (Ok(dir), Ok(path)) => dir.read(&path).await.ok(),
            _ => None,
        };
        let _ = tx.send(bytes);
        wake();
    }));
    rx
}

pub(crate) fn forget_all() {
    let Some(location) = location() else {
        return;
    };
    drop(ui::rt::app().spawn_pinned(move || async move {
        let Ok(dir) = Dir::open(location) else {
            return;
        };
        if let Ok(entries) = dir.list(&VPath::root()).await {
            for entry in entries {
                if let Ok(path) = VPath::new(&entry.name) {
                    let _ = dir.remove(&path).await;
                }
            }
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_source_round_trips_and_a_blob_is_page_local() {
        let kept = source("00ff00ff00ff00ff");
        assert_eq!(token_of(&kept), Some("00ff00ff00ff00ff"));
        assert!(!page_local(&kept));
        let blob = media::Source::Remote {
            url: "blob:http://127.0.0.1/1234".into(),
        };
        assert!(page_local(&blob));
        assert_eq!(token_of(&blob), None);
        assert_eq!(
            token_of(&media::Source::Remote {
                url: "tilo-kept:../escape".into()
            }),
            None
        );
    }
}
