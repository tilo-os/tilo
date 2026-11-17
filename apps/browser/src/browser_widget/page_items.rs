//! Filling a field from an item of its person's vault, by reference ([`crate::page_items`]).
//!
//! The action is no plain page request. The window takes the address its engine committed,
//! asks its source for the item's text at that origin, and only then takes the tab's turn,
//! with a request that carries the text once and names the origin it was released for. Until
//! the source answers nothing reaches the engine. An answer that asks the person first, a
//! refusal and a source that never answers each end the action with nothing filled.
//!
//! What the window sent this way it strikes out of everything it answers afterwards
//! ([`ui::webview::StruckTexts`]): a page may log what it was given, or send it in an address.

use std::rc::Rc;
use std::time::{Duration, Instant};

use ui::tilo_actions::{ActionInvoker, InvocationId, InvocationUpdate};
use ui::webview::{PageActor, PageRequest};

use super::{BrowserWidget, action_failed};
use crate::page_items::{ASKED_FIRST, ItemAnswer, ItemAsk, ItemSource, released_origin};

/// How long a fill waits for its item's text before it ends with nothing filled.
pub(super) const ITEM_WAIT: Duration = Duration::from_secs(30);
/// The most fills that wait for their item's text in one tab.
pub(super) const MAX_ITEM_FILLS: usize = 8;
/// The most ended fills kept for a caller that has not read its answer yet.
const MAX_ENDED: usize = 32;

/// A fill by reference whose item was asked for and has not come yet.
pub(super) struct ItemFill {
    pub(super) invocation: InvocationId,
    pub(super) actor: Option<PageActor>,
    pub(super) invoker: Option<ActionInvoker>,
    pub(super) element: String,
    /// The origin the item was asked for: the one the engine is told to compare.
    pub(super) origin: String,
    /// When the wait for the item is over.
    pub(super) until: Instant,
}

impl BrowserWidget {
    /// Hand this window its way to its person's vault: from now on it offers
    /// `fill-page-item`.
    pub fn set_item_source(&mut self, source: Rc<dyn ItemSource>) {
        self.item_source = Some(source);
        self.structure_dirty = true;
    }

    /// Start the fill of `element` from the vault item `item` names, as `invocation`: the
    /// source is asked for the item at the page's own site, and nothing reaches the engine
    /// before it answers. A refusal here names nothing of what it was given.
    pub(super) fn start_item_fill(
        &mut self,
        invocation: InvocationId,
        actor: Option<PageActor>,
        invoker: Option<&ActionInvoker>,
        element: String,
        item: String,
        reason: Option<String>,
    ) -> Result<(), String> {
        let Some(source) = self.item_source.clone() else {
            return Err(
                "This window has no way to its person's vault and fills nothing from it".into(),
            );
        };
        if !ui::webview::page::is_element_ref(&element) {
            return Err("`fill-page-item` names no element of the page outline".into());
        }
        if ui::tilo_actions::remote::person_vault::ItemRef::parse(&item).is_none() {
            return Err(
                "`fill-page-item` takes a vault item's reference, such as vault.item:<id>, and \
                 never a password or a code itself"
                    .into(),
            );
        }
        let Some(origin) = released_origin(&self.current_uri) else {
            return Err(
                "A vault item is filled only into a page that arrived over https or from this \
                 computer"
                    .into(),
            );
        };
        if self.item_fills.len() >= MAX_ITEM_FILLS {
            return Err("Too many fills from the vault wait in this tab already".into());
        }
        source.ask(ItemAsk {
            invocation,
            item,
            origin: origin.clone(),
            reason: reason.unwrap_or_default().trim().to_owned(),
            by: invoker.cloned(),
        });
        self.item_fills.push(ItemFill {
            invocation,
            actor,
            invoker: invoker.cloned(),
            element,
            origin,
            until: Instant::now() + ITEM_WAIT,
        });
        Ok(())
    }

    /// Read what the source answered the fills that wait, and take each answer on: a released
    /// text takes the tab's turn, anything else ends its action with nothing filled.
    pub(super) fn drive_item_fills(&mut self) {
        if self.item_fills.is_empty() {
            return;
        }
        let Some(source) = self.item_source.clone() else {
            return;
        };
        let now = Instant::now();
        for fill in std::mem::take(&mut self.item_fills) {
            match source.answered(fill.invocation) {
                Some(answer) => self.take_item_answer(fill, answer),
                None if now >= fill.until => {
                    source.forget(fill.invocation);
                    self.item_fill_ended(action_failed(
                        fill.invocation,
                        "browser.page.item_unanswered",
                        format!(
                            "The vault did not answer within {} seconds, so nothing was filled.",
                            ITEM_WAIT.as_secs()
                        ),
                        None,
                    ));
                }
                None => self.item_fills.push(fill),
            }
        }
    }

    fn take_item_answer(&mut self, fill: ItemFill, answer: ItemAnswer) {
        let ended = match answer {
            ItemAnswer::Released { text, into } => {
                // From here on the window strikes it out of everything it answers, whatever
                // becomes of the fill.
                self.struck.hold(&text);
                let request = PageRequest::FillReleased {
                    element: fill.element,
                    text,
                    at: fill.origin,
                    into,
                };
                let working = self.working_in_page();
                self.page_resource.admitted(
                    fill.invocation,
                    fill.actor.as_ref(),
                    fill.invoker.as_ref(),
                    &request,
                    &working,
                );
                match self.start_page_operation(fill.invocation, fill.actor, request) {
                    Ok(()) => return,
                    Err(why) => {
                        action_failed(fill.invocation, "browser.page.invalid_request", why, None)
                    }
                }
            }
            ItemAnswer::Asked { words } => action_failed(fill.invocation, ASKED_FIRST, words, None),
            ItemAnswer::Refused { reason } => {
                action_failed(fill.invocation, "browser.page.item_refused", reason, None)
            }
        };
        self.item_fill_ended(ended);
    }

    /// Keep how a fill ended before it took the tab's turn, for its caller to read.
    fn item_fill_ended(&mut self, ended: InvocationUpdate) {
        if self.item_fill_results.len() >= MAX_ENDED {
            self.item_fill_results.remove(0);
        }
        self.item_fill_results.push(ended);
    }

    /// How the fill `invocation` started ended, when it ended before it took the tab's turn.
    pub(super) fn take_item_fill_result(
        &mut self,
        invocation: InvocationId,
    ) -> Option<InvocationUpdate> {
        let index = self
            .item_fill_results
            .iter()
            .position(|ended| ended.invocation() == invocation)?;
        Some(self.item_fill_results.swap_remove(index))
    }

    /// Take the fill `invocation` started out of the wait for its item, and tell the source
    /// nobody waits for it. `false` when it does not wait here.
    pub(super) fn withdraw_item_fill(&mut self, invocation: InvocationId) -> bool {
        let Some(index) = self
            .item_fills
            .iter()
            .position(|fill| fill.invocation == invocation)
        else {
            return false;
        };
        self.item_fills.remove(index);
        if let Some(source) = &self.item_source {
            source.forget(invocation);
        }
        true
    }

    /// Strike what this window sent released out of an answer on its way to its caller.
    pub(super) fn strike_update(&self, update: &mut InvocationUpdate) {
        if self.struck.is_empty() {
            return;
        }
        match update {
            InvocationUpdate::Succeeded { output, .. } => {
                if let Some(value) = &mut output.value {
                    self.struck.strike_json(value);
                }
            }
            InvocationUpdate::Failed { error, .. } => {
                self.struck.strike(&mut error.message);
                if let Some(details) = &mut error.details {
                    self.struck.strike_json(details);
                }
            }
            InvocationUpdate::Progress {
                message: Some(message),
                ..
            }
            | InvocationUpdate::Cancelled {
                message: Some(message),
                ..
            } => {
                self.struck.strike(message);
            }
            _ => {}
        }
    }
}
