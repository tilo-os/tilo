//! The Photos app's device work: pictures the person chose, read into what the library keeps.
//!
//! A folder import is a job (`import`): for each picture the person chose, already in the content
//! store, the device checks that it is a picture it can read, reads when and where it was taken
//! from the picture's own container, and makes the small preview the grid draws. The same picture
//! chosen twice in one import is read once. The job says how far it has got as it goes, as its
//! record the app's views read, and ends saying what it read and what it could not, each with
//! whether trying again could help. Stopping it ends it there, with what it read so far.
//!
//! The app runs the same job over the pictures it already holds to read them again (a reindex),
//! and over the ones that failed to try them again.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use serde_json::json;
use tilo_content::ContentStore;
use tilolang_tilo::runtime::blob::FileValue;
use tilolang_tilo::runtime::host_work::Push;

/// The app whose engine this is.
pub const APP: &str = "so.tilo.photos";

/// The edge of the preview the grid draws, in pixels.
const PREVIEW: u32 = 512;

/// One picture the job reads: its name, where its bytes are, and where it stood in what the
/// person chose.
#[derive(Clone, Debug, Deserialize)]
struct Chosen {
    name: String,
    url: String,
    #[serde(default)]
    path: String,
}

#[derive(Debug, Deserialize)]
struct Ask {
    files: Vec<Chosen>,
}

/// A job running or ended: whether it was asked to stop.
struct Job {
    stop: AtomicBool,
}

fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Job>>> {
    static JOBS: std::sync::OnceLock<Mutex<BTreeMap<String, Arc<Job>>>> =
        std::sync::OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn next_job_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("photos-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// What makes a picture's preview: the device's preview cache over the content store.
pub type Previews = Arc<thumbnail::ThumbnailService>;

/// Start a job: `import` reads the pictures its input names, pushing its record to `push` as it
/// goes and when it ends. Answers `{"job": id}`.
///
/// # Errors
///
/// An op this engine does not run, or an input that names no pictures.
pub fn start(
    store: ContentStore,
    previews: Option<Previews>,
    op: &str,
    input: &str,
    push: Arc<dyn Push>,
) -> Result<String, String> {
    if op != "import" {
        return Err(format!("`{op}` is no op Photos runs on this device"));
    }
    let ask: Ask = serde_json::from_str(input).map_err(|why| why.to_string())?;
    let id = next_job_id();
    let job = Arc::new(Job {
        stop: AtomicBool::new(false),
    });
    jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id.clone(), Arc::clone(&job));
    let slot = id.clone();
    std::thread::Builder::new()
        .name("photos-import".to_owned())
        .spawn(move || {
            import(
                &store,
                previews.as_ref(),
                &ask.files,
                &job,
                &slot,
                push.as_ref(),
            );
            jobs()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&slot);
        })
        .map_err(|why| format!("the import could not start: {why}"))?;
    Ok(json!({ "job": id }).to_string())
}

/// Ask the job `job` to stop: it ends after the picture it is reading. False when it is not one
/// of this engine's, or it had ended.
pub fn cancel(job: &str) -> bool {
    jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(job)
        .is_some_and(|running| {
            running.stop.store(true, Ordering::SeqCst);
            true
        })
}

/// The job's work: each picture read in turn, its record pushed after each.
fn import(
    store: &ContentStore,
    previews: Option<&Previews>,
    files: &[Chosen],
    job: &Job,
    slot: &str,
    push: &dyn Push,
) {
    let mut photos = Vec::new();
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    let mut done = 0usize;
    let record =
        |state: &str, done: usize, photos: &[serde_json::Value], issues: &[serde_json::Value]| {
            json!({
                "state": state,
                "found": files.len(),
                "done": done,
                "photos": photos,
                "issues": issues,
            })
            .to_string()
        };
    push.state(slot, &record("running", 0, &[], &[]));
    for file in files {
        if job.stop.load(Ordering::SeqCst) {
            push.ended(slot, &record("stopped", done, &photos, &issues));
            return;
        }
        done = done.saturating_add(1);
        if !seen.insert(file.url.clone()) {
            // The same bytes chosen twice are one photo.
            continue;
        }
        match read(store, previews, file) {
            Ok(photo) => photos.push(photo),
            Err((why, retry)) => issues.push(json!({
                "name": file.name,
                "picture": file.url,
                "path": file.path,
                "why": why,
                "retry": retry,
            })),
        }
        push.state(slot, &record("running", done, &[], &issues));
    }
    push.ended(slot, &record("done", done, &photos, &issues));
}

/// One picture read: what the library keeps of it, or why it could not be, with whether trying
/// again could help.
fn read(
    store: &ContentStore,
    previews: Option<&Previews>,
    file: &Chosen,
) -> Result<serde_json::Value, (String, bool)> {
    let hash = file
        .url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| ("This picture is not kept on this device".to_owned(), false))?;
    let bytes = store
        .read(hash)
        .ok_or_else(|| ("Could not read this image".to_owned(), true))?;
    let info = tilo_image_codec::info::read(&bytes)
        .ok_or_else(|| ("This file is no picture Photos can read".to_owned(), false))?;
    let thumb = previews
        .and_then(|previews| {
            let value = FileValue::new(
                file.name.clone(),
                media_of(&file.name).to_owned(),
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash.to_owned(),
            );
            crate::files::thumbnail(store, previews, &value, PREVIEW)
                .ok()
                .flatten()
        })
        .map(|preview| format!("tilo-blob://{}", preview.hash))
        .unwrap_or_default();
    Ok(json!({
        "name": base_name(&file.name),
        "picture": file.url,
        "thumb": thumb,
        "folder": folder_of(&file.path),
        "captured_at": info.captured_at.unwrap_or_default(),
        "latitude": info.latitude.unwrap_or(0.0),
        "longitude": info.longitude.unwrap_or(0.0),
    }))
}

/// The file's name without its extension's folders: `trip/beach.jpg` is `beach.jpg`.
fn base_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// The folder a picture stood in within what the person chose, and empty for one on its own.
fn folder_of(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    parts
        .len()
        .checked_sub(2)
        .and_then(|at| parts.get(at))
        .map_or_else(String::new, |folder| (*folder).to_owned())
}

/// The media type a picture's name says.
fn media_of(name: &str) -> &'static str {
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("png") {
        "image/png"
    } else if extension.eq_ignore_ascii_case("webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64 × 64 PNG, real bytes the codec reads.
    fn picture() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(concat!(
                "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAAPElEQVR42u3OMQEAAAgDoC251a3g",
                "LwSgOXLVkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJ0iN1AAG5eZ4hAAAAAElFTkSu",
                "QmCC"
            ))
            .expect("the fixture is base64")
    }

    /// What the job pushed: each state, and the one it ended with.
    #[derive(Default)]
    struct Said {
        states: Mutex<Vec<String>>,
        ended: Mutex<Option<String>>,
    }

    impl Push for Said {
        fn frame(&self, _frame: tilolang_tilo::runtime::host_work::Frame) {}

        fn state(&self, _slot: &str, state: &str) {
            self.states.lock().unwrap().push(state.to_owned());
        }

        fn ended(&self, _slot: &str, state: &str) {
            *self.ended.lock().unwrap() = Some(state.to_owned());
        }
    }

    #[test]
    fn an_import_reads_each_picture_once_and_says_what_it_could_not() {
        let store = ContentStore::memory();
        let picture = store.put(&picture()).unwrap();
        let text = store.put(b"not a picture").unwrap();
        let chosen = |name: &str, hash: &str, path: &str| Chosen {
            name: name.to_owned(),
            url: format!("tilo-blob://{hash}"),
            path: path.to_owned(),
        };
        let files = vec![
            chosen("beach.png", &picture, "Trip/beach.png"),
            chosen("again.png", &picture, "Trip/again.png"),
            chosen("notes.png", &text, "Trip/notes.png"),
        ];
        let said = Said::default();
        let job = Job {
            stop: AtomicBool::new(false),
        };
        import(&store, None, &files, &job, "photos-test", &said);
        let ended: serde_json::Value =
            serde_json::from_str(said.ended.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(ended["state"], "done");
        assert_eq!(ended["found"], 3);
        assert_eq!(ended["done"], 3);
        let photos = ended["photos"].as_array().unwrap();
        assert_eq!(
            photos.len(),
            1,
            "the same picture chosen twice is one photo"
        );
        assert_eq!(photos[0]["folder"], "Trip");
        let issues = ended["issues"].as_array().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["name"], "notes.png");
        assert_eq!(issues[0]["retry"], false);
        assert!(
            said.states.lock().unwrap().len() >= 3,
            "it said how far it got as it went"
        );
    }

    #[test]
    fn a_stopped_import_ends_with_what_it_read() {
        let store = ContentStore::memory();
        let picture = store.put(&picture()).unwrap();
        let files = vec![Chosen {
            name: "beach.png".to_owned(),
            url: format!("tilo-blob://{picture}"),
            path: String::new(),
        }];
        let said = Said::default();
        let job = Job {
            stop: AtomicBool::new(true),
        };
        import(&store, None, &files, &job, "photos-test", &said);
        let ended: serde_json::Value =
            serde_json::from_str(said.ended.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(ended["state"], "stopped");
        assert_eq!(ended["done"], 0);
    }
}
