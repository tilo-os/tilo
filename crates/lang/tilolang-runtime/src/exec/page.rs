//! `page.call`: the page a window shows, and what a call answers.
//!
//! Each call is journaled as the run makes it, so a run that replays makes each one once. A
//! window that shows no page, or one that could not answer, is a value the run carries on with:
//! the answer is a JSON text saying why.

use super::{Args, Core, HResult, JournalEntry, Value, Wasm, trap};

impl Core {
    /// `page.call(request)`: the page the app's window shows does what the request says, and
    /// answers what it found, as JSON text.
    pub(crate) fn page_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let request = self.text(a.h(0)?)?;
        let answer = self.page(&request)?;
        self.ret_h(Value::text(answer))
    }

    /// The answer `request` gets, journaled: a replay answers from the journal, and a window
    /// with no page answers why.
    fn page(&self, request: &str) -> HResult<String> {
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("a page call is only made in an action");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Page {
                    request: before,
                    answer,
                } = entry
                else {
                    return trap("the run diverged from its journal: expected a page call's entry");
                };
                if before != request {
                    return trap(
                        "the run diverged from its journal: it now asks the page something else",
                    );
                }
                let answer = answer.clone();
                run.cursor = run.cursor.saturating_add(1);
                return Ok(answer);
            }
        }
        let answer = {
            let page = self.page.borrow();
            match page.as_ref() {
                Some(page) => match page.call(request) {
                    Ok(answer) => answer,
                    Err(why) => refused(&why),
                },
                None => refused("this app's window shows no page"),
            }
        };
        self.journal_push(JournalEntry::Page {
            request: request.to_owned(),
            answer: answer.clone(),
        })?;
        Ok(answer)
    }
}

/// What a call that could not be made answers: the reason in words, as the page's own answers
/// carry theirs.
fn refused(why: &str) -> String {
    serde_json::json!({ "error": why }).to_string()
}
