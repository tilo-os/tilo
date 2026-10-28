//! Bringing a chosen, pasted or dropped file into the room's shared
//! content: read within the attachment limit, stored, checked, and
//! described for the composer.

use crate::app::AttachmentImportResult;

/// What a person reads when a file is larger than a message's attachment
/// may be ([`crate::record::MAX_ATTACHMENT_BYTES`]).
pub(super) const TOO_BIG_TO_SHARE: &str = "files up to 32 MiB can be shared";

pub(super) fn prepare_attachment_import(
    content: &tilo_content::ContentStore,
    token: u64,
    path: &std::path::Path,
) -> AttachmentImportResult {
    let fail = |reason: &str| AttachmentImportResult::Failed {
        token,
        reason: reason.into(),
    };
    let Ok((dir, name)) = tilo_files::file_at(path) else {
        return fail("the file is no longer there; choose it again");
    };
    let limit = usize::try_from(crate::record::MAX_ATTACHMENT_BYTES).unwrap_or(usize::MAX);
    let bytes = match tilo_files::blocking::read_bounded(&dir, &name, limit) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return fail(TOO_BIG_TO_SHARE),
        Err(_) => return fail("the file could not be read; check it still exists"),
    };
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
    store_attachment(content, token, name, &bytes)
}

/// Keep `bytes`, a file named `name`, as shared content: what an attachment
/// refers to, whichever way the file arrived.
pub(super) fn store_attachment(
    content: &tilo_content::ContentStore,
    token: u64,
    name: String,
    bytes: &[u8],
) -> AttachmentImportResult {
    let fail = |reason: &str| AttachmentImportResult::Failed {
        token,
        reason: reason.into(),
    };
    if bytes.is_empty() {
        return fail("the file is empty");
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > crate::record::MAX_ATTACHMENT_BYTES {
        return fail(TOO_BIG_TO_SHARE);
    }
    let meta = tilo_content::sniff(bytes);
    let blob = match content.put(bytes) {
        Ok(blob) => blob,
        Err(error) => {
            eprintln!("chat: content store refused an attachment: {error}");
            return fail("there is no room to keep it here; free some space and try again");
        }
    };
    if content.read(&blob).as_deref() != Some(bytes) {
        return fail("the stored copy did not match; try again");
    }
    let video = crate::picker::is_video_name(&name);
    AttachmentImportResult::Ready {
        token,
        name,
        blob,
        size: u64::try_from(bytes.len()).unwrap_or(crate::record::MAX_ATTACHMENT_BYTES),
        width: meta.w,
        height: meta.h,
        video,
    }
}

/// Run a bounded job on its own thread, or inline where no thread can start
/// (a browser page, where the job's workspace reads fail fast). A native
/// thread that cannot start leaves the reason on the status line.
pub(super) fn run_worker(
    name: &str,
    job: impl FnOnce() + Send + 'static,
    status: &mut Option<String>,
) {
    if !ui::rt::available(ui::rt::Feature::Threads) {
        job();
        return;
    }
    if let Err(error) = ui::rt::spawn_thread(name, job) {
        *status = Some(format!("Attachments unavailable: {error}"));
    }
}
