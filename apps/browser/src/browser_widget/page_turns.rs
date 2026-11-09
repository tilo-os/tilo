//! Turns in a tab several actors work in.
//!
//! Two agents, agents on other devices, an agent and the person reviewing
//! what it did may all act in one tab. The tab still runs one page action at
//! a time: an action's answer describes the page as that action left it, and
//! the next one runs on the page the last one opened. So an action admitted
//! while another holds the tab is not refused: it waits its turn, runs in
//! the order it came and answers its own invocation.
//!
//! Waiting is bounded both ways. At most [`MAX_WAITING_PAGE_ACTIONS`] wait,
//! and the next is turned away with who holds the tab. One that has waited
//! [`PAGE_TURN_WAIT`] is answered as never run rather than left behind a tab
//! that is not moving. A caller that gives up on a waiting action takes it
//! out before it ran, so it is known to have changed nothing.
//!
//! Reading the tab's record takes no turn. It reads nothing of the page, so
//! the engine answers it at once whatever is running, and it goes to the
//! engine at once: whoever reviews a tab is never queued behind the actors
//! they are watching.

use std::time::{Duration, Instant};

use ui::webview::{PageActor, PageRequest};

use super::{
    BrowserWidget, PAGE_LOAD_WAIT, PAGE_OPERATION_TIMEOUT, PageOperation, PagePhase, action_failed,
    dialog,
};

/// How many page actions may wait their turn in one tab: room for a few
/// actors working at once, and few enough that the last one's wait is short.
pub(super) const MAX_WAITING_PAGE_ACTIONS: usize = 8;
/// How long a page action waits its turn before it is answered as never run:
/// the same wait the engine gives a request behind another.
pub(super) const PAGE_TURN_WAIT: Duration = Duration::from_secs(30);

/// How many reads of the tab's record may be with the engine at once.
pub(super) const MAX_RECORD_READS: usize = 8;

/// A read of the tab's record that is with the engine. It holds no turn.
pub(super) struct RecordRead {
    invocation: ui::tilo_actions::InvocationId,
    request_id: u32,
    /// The read as it was asked, for the answer that says what ran.
    acted: PageRequest,
    deadline: Instant,
}

/// A page action admitted while another held the tab. It has reached nothing
/// yet: no request id, no engine, no page.
pub(super) struct WaitingPage {
    pub(super) invocation: ui::tilo_actions::InvocationId,
    pub(super) actor: Option<PageActor>,
    pub(super) request: PageRequest,
    /// When it has waited as long as one may.
    pub(super) until: Instant,
}

impl BrowserWidget {
    /// Give `request` the tab if nothing holds it, or its place behind the
    /// actions that came before it.
    pub(super) fn take_page_turn(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        actor: Option<PageActor>,
        request: PageRequest,
    ) -> Result<(), String> {
        // A settled operation's update waits here until the effect reply that
        // asked for it reads it ([`Self::settle_replies`]). One whose reply is
        // gone is spent: that caller was answered `accepted` and moved on, so
        // nothing will ever read it here (`tilo_status` still answers it), and
        // holding the tab for it would keep every later action waiting. An
        // operation still in flight is never abandoned: it settles at its own
        // deadline.
        self.abandon_unwatched_page_result();
        // An action that is running holds the tab, and so does its answer
        // until its caller has read it: the action behind it runs on the page
        // that answer describes, in the order it came.
        let free = self.page_operation.is_none() && self.page_operation_result.is_none();
        if free && self.page_waiting.is_empty() {
            self.begin_page_operation(invocation, actor, request);
            return Ok(());
        }
        if self.page_waiting.len() >= MAX_WAITING_PAGE_ACTIONS {
            return Err(format!(
                "This tab has {} page actions waiting behind the one that is running ({}); \
                 ask again once they have answered",
                self.page_waiting.len(),
                self.page_turn_holder()
            ));
        }
        self.page_waiting.push_back(WaitingPage {
            invocation,
            actor,
            request,
            until: Instant::now() + PAGE_TURN_WAIT,
        });
        Ok(())
    }

    /// Read the tab's record: sent to the engine at once, beside whatever
    /// action holds the tab, and answered as its own invocation.
    pub(super) fn read_page_record(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        actor: Option<PageActor>,
        request: PageRequest,
    ) -> Result<(), String> {
        // An answer whose caller is gone is spent, as a page action's is.
        let replies = &self.pending_replies;
        self.page_record_answers.retain(|update| {
            replies
                .iter()
                .any(|(waiting, _)| *waiting == update.invocation())
        });
        if self.page_record_reads.len() >= MAX_RECORD_READS {
            return Err(format!(
                "This tab is already reading its record for {} callers; ask again once they \
                 have answered",
                self.page_record_reads.len()
            ));
        }
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        self.page_record_reads.push(RecordRead {
            invocation,
            request_id: self.page_request_seq,
            acted: request.clone(),
            deadline: Instant::now() + PAGE_OPERATION_TIMEOUT,
        });
        self.engine.send(ui::webview::WebViewCommand::Page {
            request_id: self.page_request_seq,
            actor,
            request,
        });
        Ok(())
    }

    /// Whether `request_id` is a read of the tab's record this tab sent.
    pub(super) fn reads_page_record(&self, request_id: u32) -> bool {
        self.page_record_reads
            .iter()
            .any(|read| read.request_id == request_id)
    }

    /// The engine answered a read of the tab's record: its caller's answer
    /// waits to be read, and no action's turn is touched.
    pub(super) fn accept_page_record(
        &mut self,
        request_id: u32,
        result: Result<ui::webview::PageReply, ui::webview::PageFailure>,
    ) {
        let Some(index) = self
            .page_record_reads
            .iter()
            .position(|read| read.request_id == request_id)
        else {
            return;
        };
        let read = self.page_record_reads.remove(index);
        let answer = match result {
            Ok(reply) => self.page_success(read.invocation, &reply, &read.acted, None),
            Err(failure) => action_failed(
                read.invocation,
                format!("browser.page.{}", failure.code),
                failure.message,
                Self::page_record_details(&failure.record),
            ),
        };
        self.page_record_answers.push(answer);
    }

    /// The answer to the read of the record `invocation` started, once the
    /// engine gave one or the read ran out of time.
    pub(super) fn take_page_record_answer(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        if let Some(index) = self
            .page_record_answers
            .iter()
            .position(|update| update.invocation() == invocation)
        {
            return Some(self.page_record_answers.remove(index));
        }
        let now = Instant::now();
        let index = self
            .page_record_reads
            .iter()
            .position(|read| read.invocation == invocation && now >= read.deadline)?;
        self.page_record_reads.remove(index);
        // Reading the record changes nothing, so nothing is unknown here.
        Some(action_failed(
            invocation,
            "browser.page.timeout",
            "The tab did not answer with its record in time; reading it changes nothing, so \
             ask again.",
            Some(serde_json::json!({
                "timeout_seconds": PAGE_OPERATION_TIMEOUT.as_secs(),
                "outcome": "not_run",
            })),
        ))
    }

    /// Give up on the read of the record `invocation` started: the engine is
    /// told which request nobody waits for. `false` when it reads none here.
    pub(super) fn withdraw_page_record_read(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
    ) -> bool {
        let Some(index) = self
            .page_record_reads
            .iter()
            .position(|read| read.invocation == invocation)
        else {
            return false;
        };
        let read = self.page_record_reads.remove(index);
        self.engine.send(ui::webview::WebViewCommand::CancelPage {
            request_id: read.request_id,
        });
        true
    }

    /// Start the action that has waited longest, once nothing holds the tab.
    pub(super) fn run_waiting_page_actions(&mut self) {
        while self.page_operation.is_none() && !self.page_waiting.is_empty() {
            if self.page_operation_result.is_some() {
                // An answer nobody will read must not keep an action somebody
                // waits for from its turn.
                if self
                    .page_waiting
                    .iter()
                    .any(|waiting| self.reply_outstanding(waiting.invocation))
                {
                    self.abandon_unwatched_page_result();
                }
                if self.page_operation_result.is_some() {
                    return;
                }
            }
            let Some(next) = self.page_waiting.pop_front() else {
                return;
            };
            if Instant::now() >= next.until {
                // Its turn came too late: it is answered as never run rather
                // than run for a caller that was told it would not be.
                self.page_operation_result = Some(self.waited_too_long(next.invocation));
                continue;
            }
            self.begin_page_operation(next.invocation, next.actor, next.request);
        }
    }

    /// The tab is `request`'s: it goes to the engine once its page has
    /// settled ([`Self::drive_page_operation`]).
    fn begin_page_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        actor: Option<PageActor>,
        request: PageRequest,
    ) {
        // The page runs nothing while it waits on its question: an action
        // sent now would only time out.
        if let Some(dialog) = self.page_dialogs.current() {
            let refused = dialog::blocked_output(invocation, Some(dialog));
            self.page_operation_result = Some(refused);
            return;
        }
        // A secret filled into the page is the person's credential: the page
        // holds a sign-in from the moment it is sent.
        if matches!(request, PageRequest::Fill { secret: true, .. }) {
            self.hold_sign_in();
        }
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        let act = request.described();
        let acted = request.clone();
        let now = Instant::now();
        self.page_operation = Some(PageOperation {
            invocation,
            request_id: self.page_request_seq,
            actor,
            acted,
            phase: PagePhase::Queued {
                request,
                until: now + PAGE_LOAD_WAIT,
            },
            deadline: now + PAGE_OPERATION_TIMEOUT,
            settled_while_sent: None,
            act,
        });
    }

    /// Answer the caller of `invocation` once its action has waited as long
    /// as one may. `None` while it may still get its turn, or when it does
    /// not wait here.
    pub(super) fn answer_a_page_action_that_waited_too_long(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        let now = Instant::now();
        let index = self
            .page_waiting
            .iter()
            .position(|waiting| waiting.invocation == invocation && now >= waiting.until)?;
        self.page_waiting.remove(index);
        Some(self.waited_too_long(invocation))
    }

    /// Take the action `invocation` started out of the wait before it ran.
    /// `false` when it does not wait here.
    pub(super) fn withdraw_waiting_page_action(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
    ) -> bool {
        let Some(index) = self
            .page_waiting
            .iter()
            .position(|waiting| waiting.invocation == invocation)
        else {
            return false;
        };
        self.page_waiting.remove(index);
        true
    }

    /// When the action that has waited longest has waited as long as it may,
    /// or a read of the record has, so the window wakes to answer it.
    pub(super) fn page_wait_deadline(&self) -> Option<Instant> {
        self.page_waiting
            .iter()
            .map(|waiting| waiting.until)
            .chain(self.page_record_reads.iter().map(|read| read.deadline))
            .min()
    }

    /// What an action that never got its turn is answered: it did not run,
    /// so unlike one that timed out at the page, it changed nothing.
    fn waited_too_long(
        &self,
        invocation: ui::tilo_actions::InvocationId,
    ) -> ui::tilo_actions::InvocationUpdate {
        // Never retryable: page actions are published at most once, and the
        // broker refuses a retryable failure of one.
        action_failed(
            invocation,
            "browser.page.busy",
            format!(
                "Waited {} seconds behind other page actions in this tab and never ran, so it \
                 changed nothing; running now: {}",
                PAGE_TURN_WAIT.as_secs(),
                self.page_turn_holder()
            ),
            Some(serde_json::json!({
                "waited_seconds": PAGE_TURN_WAIT.as_secs(),
                "outcome": "not_run",
            })),
        )
    }

    /// The action that holds the tab, for an actor told to wait or turned
    /// away: what it is and who asked for it.
    fn page_turn_holder(&self) -> String {
        match &self.page_operation {
            Some(operation) => match &operation.actor {
                Some(actor) => format!("{} by \"{actor}\"", operation.acted.action()),
                None => operation.acted.action().to_owned(),
            },
            None if self.page_operation_result.is_some() => {
                "an action whose answer its caller has not read yet".to_owned()
            }
            None => "nothing".to_owned(),
        }
    }
}
