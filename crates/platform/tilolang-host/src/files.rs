//! The device's files, as an app's `file.choose`, `file.save` and `.text()` reach them: the
//! device content store the bytes are kept in, and the person's own chooser.
//!
//! A chooser is the desktop host's, never one this process draws: the request goes to the
//! portal ([`tilo_dialogs`]) and the answer comes back over a channel. The instance's import is
//! synchronous, so this waits for that answer on the window's thread; the picker itself does
//! its work on the runtime's own threads, so nothing deadlocks, and the person sees a modal
//! chooser over their window while it is open.

use std::time::Duration;

use tilo_dialogs::portal::{ContentsOutcome, FileFilter, PickRequest, SaveOutcome};
use tilolang_tilo::runtime::blob::{
    FileRequest, FileValue, MAX_CHOSEN, SaveRequest, accepts, media_of,
};

/// The device content store this process was handed, when the launcher handed it one.
fn store() -> Result<tilo_content::ContentStore, String> {
    tilo_content_device::device().ok_or_else(|| "this device keeps no content store".to_owned())
}

/// The content at `hash`, when this device holds it.
pub fn read(hash: &str) -> Option<Vec<u8>> {
    tilo_content_device::device()?.read(hash)
}

/// Ask the person for a file and take it into the content store: what `file.choose` runs.
///
/// # Errors
///
/// Why there is no file: the chooser refused, or the content could not be kept.
pub fn choose(request: &FileRequest) -> Result<Option<FileValue>, String> {
    let store = store()?;
    let label = accept_label(&request.accept);
    let mut pick = PickRequest::open(&request.title, &label)
        .max_contents_bytes(MAX_CHOSEN)
        .thread_name("tilolang-choose-file");
    if !request.accept.is_empty() {
        let mut filter = FileFilter::new(&label);
        for media in &request.accept {
            filter = filter.mimetype(media);
        }
        pick = pick.filter(filter);
    }
    let chosen = match wait(&tilo_dialogs::system_picker().pick_contents(pick))? {
        ContentsOutcome::Cancelled => return Ok(None),
        ContentsOutcome::Failed(why) => return Err(why),
        ContentsOutcome::Picked(files) => files,
    };
    let Some(file) = chosen.into_iter().next() else {
        return Ok(None);
    };
    let media = file
        .media_type
        .clone()
        .unwrap_or_else(|| media_of(&file.name));
    if !accepts(&request.accept, &media) || file.bytes.len() > MAX_CHOSEN {
        // The chooser filters by type and reads no more than the limit; a host that answers
        // another file anyhow is answered for here, before anything is kept.
        return Err(format!("`{}` is not a file this asks for", file.name));
    }
    let hash = store
        .put(&file.bytes)
        .map_err(|error| format!("`{}` could not be kept: {error}", file.name))?;
    Ok(Some(FileValue::new(
        file.name,
        media,
        i64::try_from(file.bytes.len()).unwrap_or(i64::MAX),
        hash,
    )))
}

/// Put `content` where the person says: what `file.save` runs.
///
/// # Errors
///
/// Why it was not written: the chooser refused, or the write failed.
pub fn save(request: &SaveRequest, content: &[u8]) -> Result<(), String> {
    let title = format!("Save {}", request.name);
    let pick =
        PickRequest::save(&title, &request.media, &request.name).thread_name("tilolang-save-file");
    match wait(&tilo_dialogs::system_picker().save_contents(pick, content.to_vec()))? {
        SaveOutcome::Saved | SaveOutcome::Cancelled => Ok(()),
        SaveOutcome::Failed(why) => Err(why),
    }
}

/// The name a chooser shows for the types `accept` asks for.
fn accept_label(accept: &[String]) -> String {
    match accept.first().and_then(|media| media.split_once('/')) {
        Some(("image", _)) => "Images".to_owned(),
        Some(("text", _)) => "Text files".to_owned(),
        Some(("audio", _)) => "Audio".to_owned(),
        Some(("video", _)) => "Video".to_owned(),
        Some(_) => "Files".to_owned(),
        None => "Any file".to_owned(),
    }
}

/// How long a chooser may take before the answer is a failure: a person may look at their
/// files for a while, so it is generous, but a picker that never answers must not stop the
/// window for good.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// The picker's answer, or why there is none.
fn wait<T>(answers: &std::sync::mpsc::Receiver<T>) -> Result<T, String> {
    answers
        .recv_timeout(WAIT)
        .map_err(|_| "the chooser did not answer".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chooser_is_named_for_what_it_offers() {
        assert_eq!(accept_label(&[]), "Any file");
        assert_eq!(accept_label(&["image/png".to_owned()]), "Images");
        assert_eq!(accept_label(&["text/markdown".to_owned()]), "Text files");
        assert_eq!(accept_label(&["application/pdf".to_owned()]), "Files");
    }
}
