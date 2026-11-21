//! The person's file chooser, served to an app: the socket the launcher hands every app
//! ([`tilo_dialogs::shell`]) and what answers it.
//!
//! A choice of contents is answered: the person picks in the session's own chooser sheet, as
//! they pick for the portal, and the protocol keeps the bytes and hands them to the app in
//! chunks, so a sandboxed app is given the file it was given and never a host path it could not
//! reach. A request for a path is answered `Nobody`, and the app asks the chooser it brought
//! (the unattended picker, or the desktop's own), so what an app saves is unchanged from a
//! session that serves no socket at all.
//!
//! The question is the same one the portal's backend asks
//! ([`tilo_portal::file_chooser::FILE_CHOOSER_QUESTION`]): one sheet, one set of words, whether
//! an app reached it through the portal or through the shell's socket.

use std::path::PathBuf;

use tilo_dialogs::shell::{Choice, Session};
use tilo_dialogs::portal::{ContentsOutcome, PickedContents};
use tilo_portal::file_chooser::{FileChooserQuestion, FILE_CHOOSER_QUESTION, PickerFilter};
use tilo_service::{Question, Questions};

/// The session's chooser, kept for as long as the session runs: the socket answers until it
/// drops, and its file goes with it.
pub(crate) struct Chooser {
    path: PathBuf,
    _accepting: std::thread::JoinHandle<()>,
}

impl Drop for Chooser {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serve the person's chooser on the launcher's socket for it, asking `questions`.
///
/// # Errors
///
/// The services root or the socket could not be made.
pub(crate) fn serve(questions: Questions) -> Result<Chooser, String> {
    let root = launcher::services::ensure_services_root()
        .map_err(|error| format!("the services root: {error}"))?;
    serve_at(
        root.join(format!("{}.sock", launcher::services::CHOOSER_SERVICE)),
        questions,
    )
}

/// [`serve`] on `path`, which the launcher hands every app as its chooser.
fn serve_at(path: PathBuf, questions: Questions) -> Result<Chooser, String> {
    if path.exists() {
        // A socket an ended session left behind holds nothing: the launcher would hand it to an
        // app and the app would wait on a peer that is gone.
        std::fs::remove_file(&path).map_err(|error| format!("remove {}: {error}", path.display()))?;
    }
    // The socket's path may be longer than the kernel's, which a session's runtime directory
    // makes likely: the platform's bind reaches a path of any length.
    let listener = tilo_files::native::socket::bind(&path)
        .map_err(|error| format!("bind the chooser socket {}: {error}", path.display()))?;
    let accepting = std::thread::Builder::new()
        .name("tilo-chooser".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let asks = Asks {
                    questions: questions.clone(),
                    asked: std::sync::atomic::AtomicU64::new(0),
                };
                let spawned = std::thread::Builder::new()
                    .name("tilo-chooser-app".into())
                    .spawn(move || tilo_dialogs::shell::answer(stream, &asks));
                if let Err(error) = spawned {
                    tracing::warn!(%error, "an app's chooser connection was not served");
                }
            }
        })
        .map_err(|error| format!("the chooser's thread: {error}"))?;
    Ok(Chooser {
        path,
        _accepting: accepting,
    })
}

/// What the session answers an app's chooser request with: the person's pick in the session's
/// own chooser sheet, and the bytes of what they chose.
struct Asks {
    questions: Questions,
    /// Names each question apart while it waits, so one app's second question replaces its own
    /// and never another's.
    asked: std::sync::atomic::AtomicU64,
}

impl Session for Asks {
    fn choose_contents(&self, app: &str, choice: &Choice) -> ContentsOutcome {
        let filters = choice
            .filters
            .iter()
            .map(|filter| {
                let mut pairs: Vec<(u32, String)> = filter
                    .globs
                    .iter()
                    .map(|glob| (0, glob.clone()))
                    .collect();
                pairs.extend(
                    filter
                        .mimetypes
                        .iter()
                        .map(|mimetype| (1, mimetype.clone())),
                );
                PickerFilter::from_pairs(filter.name.clone(), &pairs)
            })
            .collect();
        let question = FileChooserQuestion {
            parent_window: None,
            title: choice.title.clone(),
            save: false,
            directory: false,
            writable_directory: false,
            multiple: choice.multiple,
            accept_label: (!choice.accept_label.is_empty()).then(|| choice.accept_label.clone()),
            current_name: None,
            filters,
        };
        let handle = format!(
            "chooser:{app}:{}",
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let answer = pollster::block_on(self.questions.ask(Question {
            handle,
            kind: FILE_CHOOSER_QUESTION.to_owned(),
            app_id: Some(app.to_owned()),
            label: app.to_owned(),
            details: serde_json::to_value(&question).unwrap_or_default(),
        }));
        let Some(answer) = answer else {
            return ContentsOutcome::Cancelled;
        };
        let mut files = Vec::new();
        for uri in &answer.uris {
            let Some(path) = path_of(uri) else {
                return ContentsOutcome::Failed(
                    "the chooser answered a file no app can read".to_owned(),
                );
            };
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            // Held whole, then handed over in chunks: a file past the limit would cost the
            // session its memory before the app could be told it is too large.
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() > tilo_dialogs::shell::MAX_CONTENT_BYTES => {
                    return ContentsOutcome::Failed(format!(
                        "`{name}` is {} bytes: more than the {} an app may be given at once",
                        meta.len(),
                        tilo_dialogs::shell::MAX_CONTENT_BYTES
                    ));
                }
                Ok(_) => {}
                Err(error) => {
                    return ContentsOutcome::Failed(format!("`{name}` could not be read: {error}"));
                }
            }
            match std::fs::read(&path) {
                Ok(bytes) => files.push(PickedContents {
                    name,
                    media_type: None,
                    bytes,
                    location: None,
                }),
                Err(error) => {
                    return ContentsOutcome::Failed(format!("`{name}` could not be read: {error}"));
                }
            }
        }
        if files.is_empty() {
            ContentsOutcome::Cancelled
        } else {
            ContentsOutcome::Picked(files)
        }
    }
}

/// The local file a chosen `file://` URI names; `None` for anything else.
fn path_of(uri: &str) -> Option<PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    (url.scheme() == "file")
        .then(|| url.to_file_path().ok())
        .flatten()
}
