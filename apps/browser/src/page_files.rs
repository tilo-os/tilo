//! A page's file chooser, opened by a person's own click on a file input.
//!
//! An app never draws a file picker of its own: the host's picker is the one
//! place a person chooses a file, and where their files are granted. So the
//! engine holds the page's chooser and says so, the window asks the host's
//! picker without waiting on it, and the page is answered with exactly the
//! paths the person picked there, or told nothing was chosen. The page then
//! gets what any picker's answer gives it: its own input and change events.
//!
//! The chooser is the person's alone. It is no row and no action of the
//! window, so nothing an agent does answers it or reads what was picked, and
//! a window an agent operates asks no picker at all
//! ([`BrowsingMode::offers_file_picker`]): there files reach a page through
//! `fill-page-files`, which names and checks them.
//!
//! Nothing waits for good. The engine ends a chooser whose page went
//! elsewhere, whose tab closed, or that nobody answered in its time, and says
//! so; what the picker answers after that is for no page and is dropped with
//! it. The picker itself cannot be taken back from here: it stays until the
//! person closes it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Weak};

use ui::portal::{
    FileFilter, MultiChannel, MultiOutcome, PickRequest, PortalChannel, PortalOutcome,
};
use ui::webview::file_chooser::{MAX_FILE_CHOOSER_PATH, MAX_FILE_CHOOSER_PATHS};
use ui::webview::{
    FileChooser, FileChooserMode, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent,
};

use crate::CATALOG;
use crate::browsing_mode::BrowsingMode;

/// The worker a picker that runs on a thread of its own is named by.
const WORKER: &str = "browser-page-files";

/// The host's picker, asked and not yet answered.
enum Picking {
    One(PortalChannel),
    Several(MultiChannel),
}

struct Open {
    id: u32,
    mode: FileChooserMode,
    picking: Picking,
}

/// The file chooser the page waits on, if any, while the host's picker is
/// open for it.
#[derive(Default)]
pub struct PageFiles {
    /// The page engine asked for its choosers; another engine (a restarted
    /// one) is asked again, and the chooser of the old one goes.
    asked: Option<Weak<dyn WebViewEngine>>,
    open: Option<Open>,
}

impl PageFiles {
    /// Ask `page` for the choosers a person opens, once per engine, in a
    /// window that offers the person's picker.
    pub fn sync(&mut self, page: &SharedEngine, mode: BrowsingMode) {
        let asked = self
            .asked
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|asked| Arc::ptr_eq(&asked, page));
        if asked {
            return;
        }
        // The chooser the engine before held went with it.
        self.open = None;
        self.asked = Some(Arc::downgrade(page));
        if mode.offers_file_picker() {
            page.accept_file_choosers();
        }
    }

    /// Follow what the engine said of the page's chooser: ask the host's
    /// picker for one a person opened, and let go of one the page no longer
    /// waits on.
    pub fn apply(&mut self, event: &WebViewEvent, page: &SharedEngine, mode: BrowsingMode) {
        match event {
            WebViewEvent::FileChooser(chooser) => {
                // The engine holds one chooser at a time: the one before is
                // no longer waited on, and its picker's answer is for no page.
                self.open = None;
                if !mode.offers_file_picker() {
                    page.send(WebViewCommand::AnswerFileChooser {
                        id: chooser.id,
                        paths: Vec::new(),
                    });
                    return;
                }
                self.open = Some(Open {
                    id: chooser.id,
                    mode: chooser.mode,
                    picking: ask(chooser),
                });
            }
            WebViewEvent::FileChooserClosed { id } => {
                self.open.take_if(|open| open.id == *id);
            }
            _ => {}
        }
    }

    /// Answer the page with what the person picked, once the picker says.
    pub fn poll(&mut self, page: &SharedEngine) {
        let Some(open) = &self.open else {
            return;
        };
        let closed = || Err("the picker closed without an answer".to_owned());
        let picked = match &open.picking {
            Picking::One(channel) => match channel.try_recv() {
                Ok(PortalOutcome::Opened(path)) => Ok(vec![path]),
                Ok(PortalOutcome::Cancelled) => Ok(Vec::new()),
                Ok(PortalOutcome::Failed(reason)) => Err(reason),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => closed(),
            },
            Picking::Several(channel) => match channel.try_recv() {
                Ok(MultiOutcome::Selected(paths)) => Ok(paths),
                Ok(MultiOutcome::Cancelled) => Ok(Vec::new()),
                Ok(MultiOutcome::Failed(reason)) => Err(reason),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => closed(),
            },
        };
        let (id, mode) = (open.id, open.mode);
        self.open = None;
        // All of a pick or none of it: the page never gets a part of what
        // was picked, nor a path the picker could not have given.
        let paths = picked
            .and_then(|picked| checked(&picked, mode))
            .unwrap_or_else(|reason| {
                tracing::warn!("a page's file chooser was answered with nothing: {reason}");
                Vec::new()
            });
        page.send(WebViewCommand::AnswerFileChooser { id, paths });
    }
}

/// Ask the host's picker for what `chooser` takes. The request does not
/// block: its outcome arrives on the channel.
fn ask(chooser: &FileChooser) -> Picking {
    let choose = CATALOG.t("browser-file-chooser-choose");
    let title = |own: &str| {
        if chooser.title.is_empty() {
            CATALOG.t(own)
        } else {
            chooser.title.clone()
        }
    };
    match chooser.mode {
        FileChooserMode::One => Picking::One(
            accepting(
                PickRequest::open(&title("browser-file-chooser-file"), &choose),
                &chooser.accept,
            )
            .send(),
        ),
        FileChooserMode::Several => Picking::Several(
            accepting(
                PickRequest::open_multiple(&title("browser-file-chooser-files"), &choose),
                &chooser.accept,
            )
            .send_multiple(),
        ),
        // A page is given a folder to read: never one to write into.
        FileChooserMode::Folder => Picking::One(
            PickRequest::directory(&title("browser-file-chooser-folder"), &choose)
                .thread_name(WORKER)
                .send(),
        ),
    }
}

/// `request` offering what the page's input accepts first, and every file
/// after it: what an input accepts is a hint, and nobody is kept from the
/// file they came for.
fn accepting(request: PickRequest, accept: &[String]) -> PickRequest {
    let mut accepted = FileFilter::new(&CATALOG.t("browser-file-chooser-accepted"));
    for kind in accept {
        accepted = match kind.strip_prefix('.') {
            Some(extension) if !extension.contains(['*', '?', '[', ']']) => {
                accepted.glob(&format!("*.{extension}"))
            }
            Some(_) => accepted,
            None => accepted.mimetype(kind),
        };
    }
    let request = request.thread_name(WORKER);
    if accepted.globs().is_empty() && accepted.mimetypes().is_empty() {
        request.all_files()
    } else {
        request.filter(accepted).all_files()
    }
}

/// The paths of a pick as the page's chooser takes them, or why it takes
/// none: each absolute and nameable to the engine, a file where files were
/// asked for and a folder where a folder was, and no more of them than the
/// chooser asked for. The reason never names a path: what a person picked is
/// not for a log.
fn checked(picked: &[PathBuf], mode: FileChooserMode) -> Result<Vec<String>, String> {
    let most = match mode {
        FileChooserMode::Several => MAX_FILE_CHOOSER_PATHS,
        FileChooserMode::One | FileChooserMode::Folder => 1,
    };
    if picked.len() > most {
        return Err("more was picked than the page's chooser takes".into());
    }
    picked.iter().map(|path| named(path, mode)).collect()
}

fn named(path: &Path, mode: FileChooserMode) -> Result<String, String> {
    let name = path
        .to_str()
        .filter(|name| path.is_absolute() && name.len() <= MAX_FILE_CHOOSER_PATH)
        .ok_or("a picked path is not one the page's engine can be given")?;
    // The engine opens what the picker named inside the sandbox this window
    // runs in, so opening it here is what says the engine can: nothing is
    // widened for a pick the sandbox refuses, the page is told nothing was
    // chosen. A link is followed, as the engine follows it. Only a regular
    // file is opened, so nothing that is not one can keep the window waiting.
    let fits = match mode {
        FileChooserMode::Folder => std::fs::read_dir(path).is_ok(),
        FileChooserMode::One | FileChooserMode::Several => {
            std::fs::metadata(path).is_ok_and(|kind| kind.is_file())
                && std::fs::File::open(path).is_ok()
        }
    };
    if !fits {
        return Err("what was picked is not something this window can open for the page".into());
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::webview::test_engine::TestEngine;

    fn answers(engine: &TestEngine) -> Vec<(u32, Vec<String>)> {
        engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::AnswerFileChooser { id, paths } => Some((id, paths)),
                _ => None,
            })
            .collect()
    }

    fn asks(id: u32) -> WebViewEvent {
        WebViewEvent::FileChooser(FileChooser {
            id,
            mode: FileChooserMode::One,
            title: "Upload a scan".into(),
            accept: vec![".p*f".into(), ".pdf".into()],
        })
    }

    #[test]
    fn a_restarted_engine_is_asked_again_and_the_old_ones_chooser_goes() {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("scan.pdf");
        std::fs::write(&file, b"%PDF-1.4").unwrap();
        recorder.respond_to_pick(PortalOutcome::Opened(file.clone()));

        let engine = TestEngine::new();
        let page: SharedEngine = engine.clone();
        let mut files = PageFiles::default();
        files.sync(&page, BrowsingMode::Private);
        assert!(
            engine.accepts_file_choosers(),
            "a private window is a person's too"
        );
        files.apply(&asks(1), &page, BrowsingMode::Private);
        let request = &recorder.pick_requests()[0];
        assert_eq!(request.title(), "Upload a scan", "the engine's own title");
        assert_eq!(
            request.filters()[0].globs(),
            ["*.pdf"],
            "an extension is offered as one pattern, never as one of its own"
        );

        // The engine went before the picker's answer was taken: the answer
        // is for no page, not the old engine's and not the new one's.
        let restarted = TestEngine::new();
        let page: SharedEngine = restarted.clone();
        files.sync(&page, BrowsingMode::Private);
        files.poll(&page);
        assert!(restarted.accepts_file_choosers());
        assert_eq!(answers(&engine), []);
        assert_eq!(answers(&restarted), []);

        // The new engine's own chooser is answered as any.
        recorder.respond_to_pick(PortalOutcome::Opened(file.clone()));
        files.apply(&asks(1), &page, BrowsingMode::Private);
        files.poll(&page);
        assert_eq!(
            answers(&restarted),
            [(1, vec![file.to_string_lossy().into_owned()])]
        );
        files.poll(&page);
        assert_eq!(answers(&restarted).len(), 1, "answered once");
    }
}
