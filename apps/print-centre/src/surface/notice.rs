//! How an operation went, said to the system rather than to this window: an
//! outcome that needs nobody (a printer added, a document sent, a page
//! scanned or saved) is recorded in the ledger, and one that needs a person
//! (a failure) waits in Needs you with **Try again**, which asks for the very
//! work that failed once more: the same effect on the same printer or
//! document, the same document on the same printer with the same settings,
//! the same scanner, the same page.
//!
//! Nothing here shows a notice of its own: a window says what is running
//! through [`ui::jobs`], and what happened through
//! [`ui::attention::publish`].

use std::path::PathBuf;
use std::sync::Arc;

use super::{PrintCentre, UiCommand, display_name};
use crate::resource::PrintCentreEffect;

/// The verb a failure's "Try again" carries.
pub(super) const TRY_AGAIN_ACTION: &str = "print-centre.retry";
/// The verb a failure that asking again would not change carries: open the
/// page it happened on.
pub(super) const SHOW_ACTION: &str = "print-centre.show";
/// One test at a time: the waiting stack and the ledger are the process's,
/// so two print-centre tests running at once would see each other's rows.
#[cfg(test)]
pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
    use ui::lock::MutexRecover as _;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock_recover()
}

/// The app, as the system names it when it asks for attention.
pub(super) const OWNER_APP: &str = "so.tilo.print-centre";
pub(super) const OWNER_NAME: &str = "Print centre";

/// Print centre, as the ledger and the waiting stack name it.
pub(super) fn owner() -> ui::attention::Owner {
    ui::attention::Owner::new(OWNER_APP, OWNER_NAME).with_icon(ui::icons::Icon::Printer)
}

/// What happened, as the bar says it.
#[derive(Clone)]
pub(super) struct OperationNotice {
    pub(super) text: String,
    pub(super) success: bool,
    /// The work that failed, for Try again.
    pub(super) retry: Option<Retry>,
}

impl OperationNotice {
    pub(super) fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            success: true,
            retry: None,
        }
    }

    /// A failure of work the person can ask for again as `retry`.
    pub(super) fn failure(text: impl Into<String>, retry: Retry) -> Self {
        Self {
            text: text.into(),
            success: false,
            retry: Some(retry),
        }
    }

    /// A failure that asking again would not change.
    pub(super) fn failure_only(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            success: false,
            retry: None,
        }
    }

    /// The notice this outcome is to the system: a record of what happened
    /// when nobody needs to act, an item waiting for a person when somebody
    /// does.
    fn notice(&self) -> ui::attention::Notice {
        if self.success {
            return ui::attention::Notice::did(owner(), self.text.clone());
        }
        let action = match &self.retry {
            Some(_) => ui::attention::Action::new(TRY_AGAIN_ACTION, "Try again"),
            None => ui::attention::Action::new(SHOW_ACTION, "Open the queue"),
        };
        ui::attention::Notice::needs(owner(), self.text.clone(), action)
            .urgent(ui::attention::Urgency::Waiting)
    }
}

/// Work that failed, as Try again asks for it again.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Retry {
    /// The same effect, on the same printer or document: adding,
    /// preferring, pausing or resuming a printer, cancelling or retrying a
    /// document, choosing a document to print.
    Effect(PrintCentreEffect),
    /// The same document on the same printer, with the same settings.
    Print {
        path: PathBuf,
        printer: String,
        copies: f64,
        two_sided: bool,
    },
    /// A page from the same scanner.
    Scan { scanner: String },
    /// The text of the same page.
    Recognize { page: Arc<[u8]> },
    /// Saving the same page, or every scanned page as one document.
    Save { document: bool, page: Arc<[u8]> },
}

/// The job a piece of CUPS work is: what is happening, in the person's
/// words, and about what.
pub(super) fn job_for(work: &Work) -> ui::jobs::Running {
    let (title, subject) = match work {
        Work::AddPrinter { name, .. } => ("Adding a printer", display_name(name)),
        Work::SetDefault(name) => ("Changing the default printer", display_name(name)),
        Work::SetEnabled(name, true) => ("Resuming the printer", display_name(name)),
        Work::SetEnabled(name, false) => ("Pausing the printer", display_name(name)),
        Work::Cancel(id) => ("Cancelling a document", id.clone()),
        Work::RetryJob(id) => ("Retrying a document", id.clone()),
        Work::Print { printer, path, .. } => (
            "Sending a document to the printer",
            format!(
                "{} on {}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Document"),
                display_name(printer)
            ),
        ),
    };
    ui::jobs::start(
        ui::jobs::Spec::new(title)
            .subject(ui::jobs::Subject::new(owner(), subject))
            .unit(ui::jobs::Unit::Steps)
            .phase("Talking to the printer"),
    )
}

/// What a printer or document operation asks of CUPS.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Work {
    AddPrinter {
        uri: String,
        name: String,
    },
    SetDefault(String),
    SetEnabled(String, bool),
    Cancel(String),
    RetryJob(String),
    Print {
        printer: String,
        path: PathBuf,
        settings: os_actions::printing::PrintSettings,
    },
}

impl Work {
    pub(super) fn perform(self) -> Result<(), String> {
        use os_actions::printing as cups;
        match self {
            Self::AddPrinter { uri, name } => cups::add_driverless(&uri, &name),
            Self::SetDefault(name) => cups::set_default(&name),
            Self::SetEnabled(name, enabled) => cups::set_enabled(&name, enabled),
            Self::Cancel(id) => cups::cancel_job(&id),
            Self::RetryJob(id) => cups::retry_job(&id),
            Self::Print {
                printer,
                path,
                settings,
            } => cups::print_document(&printer, &path, settings),
        }
    }
}

impl PrintCentre {
    /// Tell the system how `notice` went: recorded when nobody needs to act,
    /// waiting for the person when somebody does — and what that row's Try
    /// again asks for.
    pub(super) fn tell(&mut self, notice: OperationNotice) {
        let retry = notice.retry.clone();
        if let ui::attention::Published::Waiting(item) = ui::attention::publish(notice.notice())
            && let Some(retry) = retry
        {
            self.pending_retries.insert(item, retry);
        }
    }

    /// Carry out what the person pressed in Needs you: Try again asks for
    /// the work that failed once more, and anything else opens the queue.
    /// Returns whether anything was asked of the printer.
    pub(super) fn take_attention(&mut self) -> bool {
        let mut asked = false;
        for press in self.needs.take() {
            let Some(acted) = ui::attention::apply(press) else {
                continue;
            };
            match acted.action.verb.as_str() {
                TRY_AGAIN_ACTION => {
                    if let Some(retry) = self.pending_retries.remove(&acted.item) {
                        asked |= self.retry(retry);
                    }
                }
                _ => {
                    self.open_queue();
                    asked = true;
                }
            }
        }
        asked
    }

    /// Ask again for the work that failed as `retry` says. Returns whether
    /// anything was asked.
    pub(super) fn retry(&mut self, retry: Retry) -> bool {
        let door = self.door();
        let command = match retry {
            Retry::Effect(effect) => {
                door.effect(&effect);
                return true;
            }
            Retry::Print {
                path,
                printer,
                copies,
                two_sided,
            } => {
                self.pending_document = Some(path);
                self.selected_printer = Some(printer);
                self.print_copies.set(copies);
                self.print_two_sided.set(two_sided);
                UiCommand::PrintDocument
            }
            Retry::Scan { scanner } => {
                self.selected_scanner = Some(scanner);
                UiCommand::ScanPage
            }
            Retry::Recognize { page } => {
                if !self.select_scan_page(&page) {
                    return true;
                }
                UiCommand::RecognizeCurrentPage
            }
            Retry::Save { document, page } => {
                if !self.select_scan_page(&page) {
                    return true;
                }
                if document {
                    UiCommand::SaveScannedDocument
                } else {
                    UiCommand::SaveScannedPage
                }
            }
        };
        let _ = door.send(command);
        true
    }

    /// Put the scanned page `page` under review; whether it is still one of
    /// the document's.
    fn select_scan_page(&mut self, page: &Arc<[u8]>) -> bool {
        let Some(index) = self
            .scan
            .pages
            .iter()
            .position(|scanned| Arc::ptr_eq(&scanned.png, page))
        else {
            return false;
        };
        if index != self.scan.selected || self.scan.result.is_none() {
            self.scan.selected = index;
            self.show_selected_scan_page();
        }
        true
    }

    /// The page under review, by identity, for a retry of work on it.
    pub(super) fn reviewed_page(&self) -> Option<Arc<[u8]>> {
        self.scan.result.as_ref().map(|page| page.png.clone())
    }
}

impl super::Door {
    /// Ask for `effect` as the page's own control would.
    fn effect(&self, effect: &PrintCentreEffect) {
        if let Some(dispatcher) = self.0.as_ref() {
            dispatcher.effect(effect);
        }
    }
}

/// Stand-in devices for tests: CUPS, SANE and text recognition answer from
/// here instead of the machine the test runs on, while one is installed.
#[cfg(test)]
pub(super) mod stand_in {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub(in crate::surface) struct Devices {
        /// Whether what is asked fails.
        pub(in crate::surface) failing: bool,
        /// What was asked, in order.
        pub(in crate::surface) asked: Vec<String>,
    }

    pub(in crate::surface) type Shared = Arc<Mutex<Devices>>;

    thread_local! {
        static INSTALLED: std::cell::RefCell<Option<Shared>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Answer every request from this thread's windows from fresh stand-in
    /// devices, and hand them back.
    pub(in crate::surface) fn install() -> Shared {
        let devices = Shared::default();
        INSTALLED.with(|installed| *installed.borrow_mut() = Some(devices.clone()));
        devices
    }

    /// The stand-in devices installed on this thread, if any.
    pub(in crate::surface) fn current() -> Option<Shared> {
        INSTALLED.with(|installed| installed.borrow().clone())
    }

    /// Answer `asked` as the devices are set to: `ok` or a failure.
    pub(in crate::surface) fn answer<T>(
        devices: &Shared,
        asked: String,
        ok: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let mut devices = devices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        devices.asked.push(asked);
        if devices.failing {
            Err("the stand-in device refused".into())
        } else {
            Ok(ok())
        }
    }
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
