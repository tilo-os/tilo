//! The device's files, as an app's `file.choose`, `file.save` and `.text()` reach them: the
//! device content store the bytes are kept in, and the person's own chooser.
//!
//! Both arrive with the instance, over connections the session opens as it opens a launched
//! app's: the store is the app's in its workspace, and the chooser is the session's own. A
//! chooser is the desktop host's, never one this process draws: the request goes over that
//! connection and the answer comes back on it. The instance's import is synchronous, so this
//! waits for that answer on the window's thread; the picker itself does its work on the
//! runtime's own threads, so nothing deadlocks, and the person sees a modal chooser over
//! their window while it is open.

use std::time::Duration;

use tilo_content::ContentStore;
use tilo_dialogs::FilePicker;
use tilo_dialogs::portal::{ContentsOutcome, FileFilter, PickRequest, SaveOutcome};
use tilolang_tilo::runtime::blob::{
    FileRequest, FileValue, MAX_CHOSEN, SaveRequest, accepts, media_of,
};

/// What a person asking for a file is told when the session opened no chooser for the app: the
/// truth where nobody is at the computer, and what an action can do instead.
pub const NO_CHOOSER: &str = "nobody is at this computer to choose a file; name the file instead";

/// What a person choosing a file is told on a device that keeps no content store: the file
/// could not be kept, which is what they need to know, not the name of what is missing.
pub const NO_CONTENT_STORE: &str = "this computer cannot keep the file you chose; try again later";

/// The content at `hash`, when this device holds it.
pub fn read(store: &ContentStore, hash: &str) -> Option<Vec<u8>> {
    store.read(hash)
}

/// Ask the person for files and take them into the content store: what `file.choose` and
/// `file.choose_folder` run. One file, several, or the files of a folder, as `request` asks;
/// what comes back is an empty list when the person dismissed the chooser.
///
/// # Errors
///
/// Why there are no files: no chooser answers here, the chooser refused, or the content could
/// not be kept.
pub fn choose(
    store: &ContentStore,
    chooser: &dyn FilePicker,
    request: &FileRequest,
) -> Result<Vec<FileValue>, String> {
    // The confirm button keeps the chooser's own word (Open): the types it offers name its
    // filter, never what pressing the button does.
    let pick = if request.folder {
        PickRequest::directory(&request.title, "")
    } else if request.multiple {
        PickRequest::open_multiple(&request.title, "")
    } else {
        PickRequest::open(&request.title, "")
    };
    let mut pick = pick
        .max_contents_bytes(MAX_CHOSEN)
        .thread_name("tilolang-choose-file");
    if !request.accept.is_empty() {
        let mut filter = FileFilter::new(&filter_name(&request.accept));
        for media in &request.accept {
            filter = filter.mimetype(media);
        }
        pick = pick.filter(filter);
    }
    let chosen = match wait(&chooser.pick_contents(pick))? {
        ContentsOutcome::Cancelled => return Ok(Vec::new()),
        ContentsOutcome::Failed(why) => return Err(why),
        ContentsOutcome::Picked(files) => files,
    };
    let mut kept = Vec::with_capacity(chosen.len());
    for file in chosen {
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
        // Where the file stands in what the person chose, and what it is called there. A
        // chooser that answers a host path names only the file: a sandboxed app has no way to
        // use the directory it is in.
        let relative = file
            .location
            .as_ref()
            .and_then(|path| path.to_str())
            .filter(|path| !path.is_empty() && !std::path::Path::new(path).is_absolute());
        let name = relative
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(std::ffi::OsStr::to_str)
            .map_or_else(|| file.name.clone(), str::to_owned);
        let path = relative.map_or_else(|| name.clone(), str::to_owned);
        kept.push(
            FileValue::new(
                name,
                media,
                i64::try_from(file.bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
            .with_path(path),
        );
    }
    Ok(kept)
}

/// Keep `bytes` in the content store as a file called `name`: what a program that holds bytes
/// makes of them, such as the files an archive a person chose holds.
///
/// # Errors
///
/// Why the bytes were not kept: too large a file, or the store's failure.
pub fn keep(
    store: &ContentStore,
    name: &str,
    media: &str,
    bytes: &[u8],
) -> Result<FileValue, String> {
    if bytes.len() > MAX_CHOSEN {
        return Err(format!(
            "`{name}` is {} bytes: more than the {MAX_CHOSEN} a file may be",
            bytes.len()
        ));
    }
    let hash = store
        .put(bytes)
        .map_err(|error| format!("`{name}` could not be kept: {error}"))?;
    Ok(FileValue::new(
        name,
        media,
        i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        hash,
    ))
}

/// A preview of `file` no larger than `size` pixels a side, kept in the content store: what
/// `File.thumbnail` answers, generated once by the device's shared cache and byte for byte the
/// same on every call. `None` for a file that has no preview, which is no image (a badge is what
/// its list shows instead).
///
/// # Errors
///
/// Why there is no preview: a file that is not addressed content, or a preview that did not
/// encode or could not be kept.
pub fn thumbnail(
    store: &ContentStore,
    thumbnails: &thumbnail::ThumbnailService,
    file: &FileValue,
    size: u32,
) -> Result<Option<FileValue>, String> {
    let category = thumbnail::category_for_path(std::path::Path::new(&file.name));
    if !category.has_preview() {
        return Ok(None);
    }
    let edge = size.clamp(1, 4096);
    let dimension = thumbnail::ThumbSize::edge(edge);
    let Some(key) = thumbnail::ThumbKey::of_address(&file.hash, dimension) else {
        return Err(format!(
            "`{}` is not the address of content this device holds",
            file.name
        ));
    };
    let state = thumbnails.request(thumbnail::ThumbRequest::load(key, category, {
        let store = store.clone();
        let hash = file.hash.clone();
        move || store.read(&hash)
    }));
    let preview = match state {
        thumbnail::ThumbState::Ready(preview) => Some(preview),
        thumbnail::ThumbState::Badge(_) => None,
        thumbnail::ThumbState::Loading => {
            // The cache generates on its own workers; a preview is ready before the call
            // answers, as a file read is. One that never settles answers no preview.
            thumbnails.block_until_idle(Duration::from_secs(30));
            thumbnails
                .peek(&key)
                .and_then(|state| state.ready().cloned())
        }
    };
    let Some(preview) = preview else {
        return Ok(None);
    };
    let png = thumbnail::encode_png(&preview)
        .ok_or_else(|| format!("the preview of `{}` did not encode", file.name))?;
    let stem = std::path::Path::new(&file.name)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("preview");
    keep(store, &format!("{stem}-{edge}.png"), "image/png", &png).map(Some)
}

/// Put `content` where the person says: what `file.save` runs.
///
/// # Errors
///
/// Why it was not written: the chooser refused, or the write failed.
pub fn save(chooser: &dyn FilePicker, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
    let title = format!("Save {}", request.name);
    // The confirm button reads Save, not the file's media type.
    let pick = PickRequest::save(&title, "", &request.name).thread_name("tilolang-save-file");
    match wait(&chooser.save_contents(pick, content.to_vec()))? {
        SaveOutcome::Saved | SaveOutcome::Cancelled => Ok(()),
        SaveOutcome::Failed(why) => Err(why),
    }
}

/// The name a chooser's filter shows for the types `accept` asks for.
fn filter_name(accept: &[String]) -> String {
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
    use tilo_dialogs::portal::{
        ContentsChannel, LaunchChannel, LaunchRequest, MultiChannel, OpenFileChannel, PickRequest,
        PickedContents, PortalChannel,
    };

    use super::*;

    /// A channel nothing will answer: the requests a test never makes.
    fn unanswered<T>() -> std::sync::mpsc::Receiver<T> {
        std::sync::mpsc::channel().1
    }

    #[test]
    fn a_choosers_filter_is_named_for_what_it_offers() {
        assert_eq!(filter_name(&[]), "Any file");
        assert_eq!(filter_name(&["image/png".to_owned()]), "Images");
        assert_eq!(filter_name(&["text/markdown".to_owned()]), "Text files");
        assert_eq!(filter_name(&["application/pdf".to_owned()]), "Files");
    }

    /// A chooser that answers every contents pick with `name` and `bytes`: the person picking a
    /// file, without a desktop.
    struct Picks(
        Vec<(&'static str, &'static [u8])>,
        std::sync::Mutex<Option<PickRequest>>,
    );

    impl Picks {
        fn new(name: &'static str, bytes: &'static [u8]) -> Self {
            Self(vec![(name, bytes)], std::sync::Mutex::new(None))
        }

        fn several(files: Vec<(&'static str, &'static [u8])>) -> Self {
            Self(files, std::sync::Mutex::new(None))
        }
    }

    impl FilePicker for Picks {
        fn pick(&self, _: PickRequest) -> PortalChannel {
            unanswered()
        }

        fn pick_multiple(&self, _: PickRequest) -> MultiChannel {
            unanswered()
        }

        fn open_file(&self, _: PickRequest) -> OpenFileChannel {
            unanswered()
        }

        fn launch(&self, _: LaunchRequest) -> LaunchChannel {
            unanswered()
        }

        fn pick_contents(&self, request: PickRequest) -> ContentsChannel {
            *self.1.lock().unwrap() = Some(request);
            let (sender, receiver) = std::sync::mpsc::channel();
            let files = self
                .0
                .iter()
                .map(|(name, bytes)| PickedContents {
                    name: (*name).to_owned(),
                    media_type: Some(media_of(name)),
                    bytes: bytes.to_vec(),
                    location: Some(std::path::PathBuf::from(name)),
                })
                .collect();
            let _ = sender.send(ContentsOutcome::Picked(files));
            receiver
        }
    }

    #[test]
    fn a_chosen_file_is_kept_in_the_store_the_instance_holds() {
        let store = ContentStore::memory();
        let chooser = Picks::new("Lisbon.md", b"# Lisbon in May\n");
        let chosen = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import Markdown".to_owned(),
                name: String::new(),
                accept: vec!["text/markdown".to_owned()],
                ..FileRequest::default()
            },
        )
        .expect("the chooser answered");
        assert_eq!(chosen.len(), 1, "one file was chosen");
        let chosen = &chosen[0];
        assert_eq!(chosen.name, "Lisbon.md");
        assert_eq!(chosen.path, "Lisbon.md", "the file's own name is its path");
        assert_eq!(chosen.media, "text/markdown");
        let asked = chooser
            .1
            .lock()
            .unwrap()
            .take()
            .expect("the chooser was asked");
        assert_eq!(
            (asked.title(), asked.accept_label(), asked.filter_names()),
            ("Import Markdown", "", vec!["Text files"]),
            "the types name the filter; the confirm button keeps the chooser's own Open"
        );
        assert_eq!(
            store.read(&chosen.hash).as_deref(),
            Some(&b"# Lisbon in May\n"[..]),
            "the bytes the person chose are in the instance's own store"
        );
    }

    #[test]
    fn a_file_another_type_is_refused_before_it_is_kept() {
        let store = ContentStore::memory();
        let chooser = Picks::new("photo.png", b"\x89PNG");
        let refused = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import Markdown".to_owned(),
                name: String::new(),
                accept: vec!["text/markdown".to_owned()],
                ..FileRequest::default()
            },
        );
        assert_eq!(
            refused,
            Err("`photo.png` is not a file this asks for".to_owned())
        );
        assert!(
            !store.has(&tilo_content::hash_bytes(b"\x89PNG")),
            "nothing was kept"
        );
    }

    #[test]
    fn several_files_and_a_folders_files_are_kept_with_their_paths() {
        let store = ContentStore::memory();
        let chooser = Picks::several(vec![
            ("trip/one.md", b"# One\n"),
            ("trip/two.md", b"# Two\n"),
        ]);
        let chosen = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import notes".to_owned(),
                accept: vec!["text/markdown".to_owned()],
                multiple: true,
                ..FileRequest::default()
            },
        )
        .expect("the chooser answered");
        assert_eq!(
            chosen
                .iter()
                .map(|file| (file.name.as_str(), file.path.as_str()))
                .collect::<Vec<_>>(),
            [("one.md", "trip/one.md"), ("two.md", "trip/two.md")],
            "each file keeps its name and its path inside what the person chose"
        );
        let asked = chooser.1.lock().unwrap().take().expect("asked");
        assert!(asked.is_multiple(), "the person may choose several");

        let chooser = Picks::several(vec![("trip/photo.png", b"png")]);
        let chosen = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import a folder".to_owned(),
                accept: vec!["image/*".to_owned()],
                folder: true,
                ..FileRequest::default()
            },
        )
        .expect("the chooser answered");
        assert_eq!(chosen.len(), 1);
        let asked = chooser.1.lock().unwrap().take().expect("asked");
        assert!(asked.is_directory(), "the person chooses a folder");
    }

    #[test]
    fn a_preview_comes_from_the_devices_cache_and_keeps_a_file_of_its_own() {
        let image = thumbnail::RgbaThumb {
            rgba: [20_u8, 40, 60, 255].repeat(32 * 16),
            width: 32,
            height: 16,
        };
        let png = thumbnail::encode_png(&image).expect("the fixture encodes");
        let store = ContentStore::memory();
        let hash = store.put(&png).expect("the fixture is kept");
        let file = FileValue::new(
            "photo.png",
            "image/png",
            i64::try_from(png.len()).unwrap_or(i64::MAX),
            hash,
        );
        let service = thumbnail::ThumbnailService::ephemeral(1);
        let preview = thumbnail(&store, &service, &file, 16)
            .expect("the preview generated")
            .expect("an image has a preview");
        assert_eq!(preview.name, "photo-16.png");
        assert_eq!(preview.media, "image/png");
        let bytes = store.read(&preview.hash).expect("the preview is kept");
        let decoded = thumbnail::decode_png(&bytes).expect("the preview is a PNG");
        assert_eq!(decoded.width.max(decoded.height), 16);

        // A file that is no image has none: its list shows a badge instead.
        let text = FileValue::new(
            "plan.md",
            "text/markdown",
            4,
            store.put(b"# P\n").expect("the text is kept"),
        );
        assert!(
            thumbnail(&store, &service, &text, 16)
                .expect("no preview is no error")
                .is_none()
        );
    }

    #[test]
    fn bytes_a_program_makes_are_kept_too() {
        let store = ContentStore::memory();
        let kept = keep(&store, "note.md", "text/markdown", b"hello").expect("kept");
        assert_eq!(store.read(&kept.hash).as_deref(), Some(&b"hello"[..]));
        let oversized = vec![0_u8; MAX_CHOSEN + 1];
        assert!(keep(&store, "big.zip", "application/zip", &oversized).is_err());
    }
}
