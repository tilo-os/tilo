//! How an operation went, said in the window bar's notice
//! ([`ui::window_toolbar`]): a printer added, a document sent, a page
//! scanned or saved. A success only informs. A failure carries Try again,
//! which asks for the very work that failed once more: the same effect on
//! the same printer or document, the same document on the same printer with
//! the same settings, the same scanner, the same page.

use std::path::PathBuf;
use std::sync::Arc;

use ui::widget::toast::ToastKind;
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::{PrintCentre, UiCommand};
use crate::resource::PrintCentreEffect;

/// The bar action id of a failure's "Try again".
pub(super) const TRY_AGAIN_ACTION: &str = "print-centre.notice.try-again";

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

    fn bar_notice(&self) -> ToolbarNotice {
        let kind = if self.success {
            ToastKind::Success
        } else {
            ToastKind::Error
        };
        let notice = ToolbarNotice::new(self.text.clone(), kind);
        if self.retry.is_some() {
            notice.action(ToolbarAction::new(
                TRY_AGAIN_ACTION,
                "rotate-cw",
                "Try again",
            ))
        } else {
            notice
        }
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
    /// Say `notice` in the bar, replacing whatever was said.
    pub(super) fn tell(&mut self, notice: OperationNotice) {
        self.notice.show(notice);
    }

    /// Keep the bar's notice saying how the last operation went while it
    /// lasts; call on every sync.
    pub(super) fn publish_notice(&mut self) {
        self.notice.expire();
        self.notice.publish(OperationNotice::bar_notice);
    }

    /// Ask again for the work whose failure is up, when its Try again was
    /// pressed. Returns whether it was.
    pub(super) fn take_try_again(&mut self) -> bool {
        let Some(retry) = self.notice.get().and_then(|notice| notice.retry.clone()) else {
            return false;
        };
        if !ui::window_toolbar::take_notice_press(TRY_AGAIN_ACTION) {
            return false;
        }
        self.notice.dismiss();
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
