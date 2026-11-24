//! The shell's chooser service (`tilo_dialogs::shell`): one system service every app reaches for
//! the files it asks the person for and the links and files it opens.
//!
//! The launcher connects an app to this socket before its sandbox starts and writes the first
//! line, a [`Handshake`] naming the app and its workspace; nothing the app writes after can change
//! who it is. The server itself is portable ([`tilo_dialogs::chooser_service`]): this module fills
//! it in with the session's own questions ([`tilo_portal::file_chooser::choose`]) and with the
//! loop that opens a link or a file in the app that takes it ([`crate::apps::Apps::open_claimed`]).

use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::ApiError;
use tilo_dialogs::chooser_service::{
    Asked, Caller, Opens as ServiceOpens, Person, Question, Serving as ServiceServing,
};
use tilo_lock::MutexRecover as _;
use tilo_portal::PortalCaller;
use tilo_portal::file_chooser::{FileChooserQuestion, PickerFilter};
use tilo_portal::reply::Ended;
use tilo_service::{AskSender, Wake};

/// How long an open waits for the session's loop, which alone starts apps. The app waits a little
/// longer for the answer than this.
const OPEN_WAIT: Duration = Duration::from_secs(20);

const DID_NOT_OPEN: &str = "It didn't open. Try again.";
const NOT_IN_TIME: &str = "It didn't open in time. Try again.";

/// What the chooser service is served with.
pub(crate) struct Serving {
    /// Where the person is asked; `None` when nobody is here to answer one.
    pub questions: Option<AskSender>,
    /// The files people choose for apps, while the session serves them to its sandboxes.
    pub documents: tilo_documents::Documents,
    /// What wakes the session's loop when an open waits for it.
    pub wake: Wake,
}

/// An open the service leaves for the session's loop: only a pass of the loop decides which app
/// takes a link and starts it.
struct Open {
    workspace: String,
    uri: String,
    reply: SyncSender<Result<Option<()>, ApiError>>,
}

/// What opens a link or a file for the loop: the session's apps.
pub(crate) trait Opens {
    /// Open `uri` in `workspace` in the app that takes it; `None` when nothing does.
    ///
    /// # Errors
    /// The app that takes it did not start.
    fn open_claimed(&self, workspace: &str, uri: &str) -> Result<Option<()>, ApiError>;
}

impl Opens for crate::apps::Apps {
    fn open_claimed(&self, workspace: &str, uri: &str) -> Result<Option<()>, ApiError> {
        Self::open_claimed(self, workspace, uri)
    }
}

/// The opens the connection threads leave for the loop.
#[derive(Default)]
struct Pending {
    opens: Mutex<Vec<Open>>,
    wake: Mutex<Option<Wake>>,
}

impl Pending {
    fn push(&self, open: Open) {
        self.opens.lock_recover().push(open);
        if let Some(wake) = self.wake.lock_recover().as_ref() {
            wake();
        }
    }
}

impl ServiceOpens for Pending {
    fn open(&self, workspace: &str, uri: &str) -> Result<Option<()>, String> {
        let (reply, answered) = std::sync::mpsc::sync_channel(1);
        self.push(Open {
            workspace: workspace.to_owned(),
            uri: uri.to_owned(),
            reply,
        });
        match answered.recv_timeout(OPEN_WAIT) {
            Ok(Ok(opened)) => Ok(opened),
            Ok(Err(error)) => {
                tracing::warn!(%error, "what an app asked to open did not open");
                Err(DID_NOT_OPEN.to_owned())
            }
            Err(_) => Err(NOT_IN_TIME.to_owned()),
        }
    }
}

/// The chooser service, served until the session ends.
pub(crate) struct Served {
    pending: Arc<Pending>,
}

impl Served {
    /// Open what the apps asked to open since the last pass, on the loop.
    pub(crate) fn follow(&self, opens: &dyn Opens) {
        let asked = std::mem::take(&mut *self.pending.opens.lock_recover());
        for open in asked {
            let _ = open
                .reply
                .send(opens.open_claimed(&open.workspace, &open.uri));
        }
    }
}

/// Bind the chooser socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
pub(crate) fn serve(serving: Serving) -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    serve_at(
        &launcher::services::socket_for(launcher::services::CHOOSER_SERVICE),
        serving,
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the chooser service accepts and serves on its own threads"
)]
fn serve_at(path: &std::path::Path, serving: Serving) -> Result<Served, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove the stale chooser socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(path)
        .map_err(|error| format!("bind the chooser socket {}: {error}", path.display()))?;
    let pending = Arc::new(Pending {
        opens: Mutex::new(Vec::new()),
        wake: Mutex::new(Some(serving.wake)),
    });
    let shared = ServiceServing {
        person: serving
            .questions
            .map(|questions| Arc::new(SessionPerson { questions }) as Arc<dyn Person>),
        documents: serving.documents,
        opens: Arc::clone(&pending) as Arc<dyn ServiceOpens>,
    };
    let serving = Arc::new(shared);
    std::thread::Builder::new()
        .name("tilo-chooser".into())
        .spawn(move || tilo_dialogs::chooser_service::serve_listener(&listener, &serving))
        .map_err(|error| format!("start the chooser service: {error}"))?;
    tracing::info!(path = %path.display(), "choosing files and opening links for the apps");
    Ok(Served { pending })
}

/// The session's own file chooser, asked as the desktop portal asks it.
struct SessionPerson {
    questions: AskSender,
}

impl Person for SessionPerson {
    fn choose(
        &self,
        caller: &Caller,
        handle: String,
        question: &Question,
    ) -> Result<Vec<PathBuf>, Asked> {
        let questions = self.questions.clone();
        let caller = PortalCaller {
            workspace: Some(caller.workspace.clone()),
            sandboxed: false,
            ..PortalCaller::new(caller.app.clone())
        };
        let question = file_chooser_question(question);
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = async move {
            let _ = tx.send(
                tilo_portal::file_chooser::choose(&questions, &caller, handle, &question).await,
            );
        };
        match tilo_rt::try_app() {
            Ok(runtime) => {
                runtime.spawn(asked);
            }
            Err(error) => {
                tracing::warn!(%error, "no task asks the person for a file");
                return Err(Asked::Failed);
            }
        }
        match rx.recv() {
            Ok(Ok(paths)) => Ok(paths),
            Ok(Err(Ended::Cancelled)) => Err(Asked::Cancelled),
            Ok(Err(Ended::Failed)) | Err(_) => Err(Asked::Failed),
        }
    }
}

/// The question a chooser asks, as the session's file chooser shows it.
fn file_chooser_question(question: &Question) -> FileChooserQuestion {
    FileChooserQuestion {
        parent_window: None,
        title: question.title.clone(),
        save: question.save,
        directory: question.directory,
        writable_directory: question.writable_directory,
        multiple: question.multiple,
        accept_label: question.accept_label.clone(),
        current_name: question.current_name.clone(),
        filters: question.filters.iter().map(picker_filter).collect(),
    }
}

/// A filter the app named, as the session's file chooser shows one.
fn picker_filter(filter: &tilo_dialogs::portal::FileFilter) -> PickerFilter {
    let pairs: Vec<(u32, String)> = filter
        .globs()
        .iter()
        .map(|glob| (0, glob.clone()))
        .chain(filter.mimetypes().iter().map(|mime| (1, mime.clone())))
        .collect();
    PickerFilter::from_pairs(filter.name().to_owned(), &pairs)
}

#[cfg(test)]
mod tests;
