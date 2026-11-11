//! The tab's record, kept in Browser's resource of page actions
//! ([`crate::page_record`]).
//!
//! The engine sets an operating action down as it ends and says so in the
//! action's answer. The window then reads what the engine listed since it
//! last looked and writes each action into the resource, as its actor. What
//! a person types straight into the page no answer carries: the engine says
//! it set that down, and the window writes it as the person's. A
//! read of the tab's record answers from the resource, so it answers the
//! same after the engine started again, and adds what only the engine knows
//! at that moment: what is running and how many wait.

use std::time::Instant;

use ui::webview::{
    PageFailure, PageNavigation, PageRecordNote, PageReply, PageRequest, WebViewCommand,
};

use super::{BrowserWidget, PAGE_OPERATION_TIMEOUT};
use crate::page_record::PageRecord;

impl BrowserWidget {
    /// Keep this tab's record in `record`, Browser's record of page actions
    /// in the workspace it is open in, as a tab of the device `device`.
    pub(crate) fn attach_page_record(
        &mut self,
        record: ui::framework::PersonRecord<PageRecord>,
        device: &str,
    ) {
        self.page_record.attach(record, device);
    }

    /// The page a window saves to show again, with the tab whose record it
    /// goes on with when it reopens on this device.
    pub(crate) fn saved_with_page_record(&self, saved: Vec<u8>) -> Vec<u8> {
        let Some(tab) = self.page_record.tab() else {
            return saved;
        };
        let Ok(serde_json::Value::Object(mut page)) = serde_json::from_slice(&saved) else {
            return saved;
        };
        page.insert("tab".into(), tab.into());
        serde_json::to_vec(&page).unwrap_or(saved)
    }

    /// Go on with the record of the tab a reopened window showed.
    pub(crate) fn restore_page_record(&mut self, saved: &[u8]) {
        if let Ok(page) = serde_json::from_slice::<serde_json::Value>(saved)
            && let Some(tab) = page.get("tab").and_then(serde_json::Value::as_str)
        {
            self.page_record.adopt(tab);
        }
    }

    /// `operation` was admitted for `invoker`, who acts in the tab as
    /// `actor`: what the engine sets down under that name is theirs. A form's
    /// own action is its invoker's as a page action is, so the answers it
    /// places and the submission the window holds from it are kept under the
    /// name of whoever ran it, authored by them.
    pub(super) fn admitted_to_page(
        &mut self,
        actor: Option<&ui::webview::PageActor>,
        invoker: Option<&ui::tilo_actions::ActionInvoker>,
        operation: &crate::effects::BrowserOperation,
    ) {
        match operation {
            crate::effects::BrowserOperation::Page(request) => {
                self.page_record.admitted(actor, invoker, request);
            }
            crate::effects::BrowserOperation::Form(_) => self.page_record.acts_as(actor, invoker),
            _ => {}
        }
    }

    /// The engine the page runs in now, told apart by where it lives.
    fn page_engine(&self) -> usize {
        std::sync::Arc::as_ptr(&self.engine).cast::<()>().addr()
    }

    /// An answer of the engine's, before the window acts on it. `None` for
    /// the answer to a read of the engine's record this window sent for
    /// itself: what it lists is written into the resource and nobody else
    /// hears of it. Any other answer names its places as the resource
    /// counts them, and one that says an action was set down has the window
    /// read what the engine listed.
    pub(super) fn record_page_result(
        &mut self,
        request_id: u32,
        result: Result<PageReply, PageFailure>,
    ) -> Option<Result<PageReply, PageFailure>> {
        if !self.page_record.attached() {
            return Some(result);
        }
        self.page_record.engine_is(self.page_engine());
        if self.page_record.pulled(request_id) {
            let more = result
                .as_ref()
                .is_ok_and(|reply| self.page_record.set_down(reply.data.as_ref()));
            if more || self.page_record.take_again() {
                self.pull_page_record();
            }
            return None;
        }
        // A caller's own read of the record is answered from the resource
        // ([`Self::page_record_reply`]).
        if self.reads_page_record(request_id) {
            return Some(result);
        }
        let (result, news) = self.page_record.placed(result);
        if news {
            self.pull_page_record();
        }
        Some(result)
    }

    /// The engine set `entry` down at its place `seq` and no action's
    /// answer carries it: what a person typed straight into the page, or
    /// what the window held the page from sending and how that ended. It
    /// joins the tab's record in its place, as its actor's, and as the
    /// person's when it names none.
    pub(super) fn hear_page_record(&mut self, seq: u64, entry: &serde_json::Value) {
        if self.page_record.attached() {
            self.page_record.engine_is(self.page_engine());
            if self.page_record.heard(seq, entry) {
                self.pull_page_record();
            }
        }
        // The tab's record moved under the page's review.
        self.page_review_moved();
    }

    /// Ask the engine what it set down since this window last read it. It
    /// reads nothing of the page, so it takes no turn.
    fn pull_page_record(&mut self) {
        let now = Instant::now();
        if self.page_record.pulling(now) {
            return;
        }
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        self.page_record
            .pull_sent(self.page_request_seq, now + PAGE_OPERATION_TIMEOUT);
        self.engine.send(WebViewCommand::Page {
            request_id: self.page_request_seq,
            actor: None,
            request: PageRequest::Ledger {
                since: self.page_record.cursor(),
            },
        });
    }

    /// What the engine is asked for a caller's read of the tab's record:
    /// what it set down since this window last read it, whatever place the
    /// caller reads on from. The caller's place is answered by the resource.
    pub(super) fn page_record_request(&mut self, asked: PageRequest) -> PageRequest {
        if !self.page_record.attached() || !matches!(asked, PageRequest::Ledger { .. }) {
            return asked;
        }
        self.page_record.engine_is(self.page_engine());
        PageRequest::Ledger {
            since: self.page_record.cursor(),
        }
    }

    /// The answer to a caller's read of the tab's record, `asked`, from the
    /// resource: what the engine listed is written first, then the caller's
    /// part of the record is read there. What only the engine knows now
    /// (the action running, how many wait behind it) is taken from its
    /// answer. An engine that did not answer leaves the record as readable
    /// as it was.
    pub(super) fn page_record_reply(
        &mut self,
        asked: &PageRequest,
        engine: Result<PageReply, PageFailure>,
    ) -> Result<PageReply, PageFailure> {
        let PageRequest::Ledger { since } = asked else {
            return engine;
        };
        if !self.page_record.attached() {
            return engine;
        }
        self.page_record.engine_is(self.page_engine());
        let more = engine
            .as_ref()
            .is_ok_and(|reply| self.page_record.set_down(reply.data.as_ref()));
        if more {
            self.pull_page_record();
        }
        let Some(serde_json::Value::Object(mut data)) = self.page_record.read(*since, more) else {
            return engine;
        };
        let listed = data
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        let further = data.get("more").and_then(serde_json::Value::as_bool) == Some(true);
        let mut message = format!(
            "{listed} {}{}",
            if listed == 1 { "action" } else { "actions" },
            if further { ", and more after them" } else { "" }
        );
        let live = engine.as_ref().ok().and_then(|reply| reply.data.as_ref());
        data.insert(
            "waiting".into(),
            live.and_then(|live| live.get("waiting"))
                .cloned()
                .unwrap_or_else(|| 0.into()),
        );
        if let Some(running) = live.and_then(|live| live.get("running")) {
            data.insert("running".into(), self.page_record.running(running));
        }
        let data = serde_json::Value::Object(data);
        Ok(match engine {
            Ok(mut reply) => {
                self.page_record.places(&mut reply.record);
                PageReply {
                    message,
                    data: Some(data),
                    ..reply
                }
            }
            Err(failure) => {
                message.push_str("; the page's engine did not answer (");
                message.push_str(&failure.code);
                message.push_str("), so what is running now is not known");
                PageReply {
                    url: self.current_uri.clone(),
                    message,
                    outline: None,
                    data: Some(data),
                    image: None,
                    navigation: PageNavigation::None,
                    loading: false,
                    record: PageRecordNote::default(),
                }
            }
        })
    }
}
