//! The conversation's running summary, kept while a turn is answered.
//!
//! A turn whose conversation outgrew what it is told as written carries a fold
//! ([`runtime_collab::recall::Fold`]): the summary so far and the oldest messages outside it. The
//! agent asks a model once for what those messages add and keeps the summary through Chat's own
//! `keep-summary`, as a step of the turn.
//!
//! It does so once the reply has its words, so the person never waits for it. Nothing here can
//! fail a turn: no model, a call that failed or ran out of time, a workspace that offers no
//! `keep-summary`, or the person writing again meanwhile all leave the summary as it stood, and
//! the next turn that still sees the messages outside it asks again.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};

use agent_provider_config::Interaction;
use anyhow::Result;
use runtime_collab::recall::Fold;
use serde_json::{Map, json};
use ui::time::Duration;

use super::{Asked, Session, TaskAction};

/// The chat action that keeps a conversation's summary.
const KEEP_SUMMARY: &str = "keep-summary";
/// What separates a message's own id from its conversation's address, where the turn's item is
/// published.
const MESSAGES: &str = "/messages/";
/// How long the one question may take.
const ASK_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the wait for its answer looks whether the person ended the turn.
const LOOK_EVERY: Duration = Duration::from_millis(50);

impl Session {
    /// Take what `run` asks into its conversation's summary: one question to a model, and one
    /// `keep-summary` with what is kept of the answer.
    ///
    /// # Errors
    /// Only this session's own I/O failing.
    pub(super) fn keep_summary(&mut self, run: &Asked) -> Result<()> {
        let Some(fold) = run.fold.clone() else {
            return Ok(());
        };
        // Where the answer could not be kept, no model is asked.
        let Some(action) = self.keep_summary_action(run, &fold) else {
            tracing::info!(
                conversation = %fold.conversation,
                "the workspace offers no `keep-summary` on the turn: the summary stays as it is"
            );
            return Ok(());
        };
        let Some(answer) = self.ask_once(run, &fold) else {
            return Ok(());
        };
        let mut arguments = action.arguments.clone();
        arguments.insert("text".into(), json!(fold.kept(&answer)));
        match self.start_task_invocation(run, &action, &arguments)? {
            Ok(following) => {
                let mut followed = [following];
                self.follow_native(run, &mut followed)?;
                if let Some(Err(stop)) = followed.first().and_then(|kept| kept.outcome.as_ref()) {
                    tracing::warn!(?stop, "the conversation did not keep its summary");
                } else {
                    tracing::info!(
                        conversation = %fold.conversation,
                        through = %fold.through,
                        rewritten = fold.condenses(),
                        "kept the conversation's summary"
                    );
                }
            }
            Err(stop) => tracing::warn!(?stop, "the conversation refused its summary"),
        }
        Ok(())
    }

    /// Chat's `keep-summary` as the broker offers it on the turn's message, with everything it
    /// takes but the summary's text. Absent where the workspace offers none there.
    fn keep_summary_action(&mut self, run: &Asked, fold: &Fold) -> Option<TaskAction> {
        let (_, message) = run.message.rsplit_once(MESSAGES)?;
        let item = format!("{MESSAGES}{message}");
        let offer = self
            .task_catalogue()
            .offers
            .iter()
            .find(|offer| {
                offer.descriptor.address.action == KEEP_SUMMARY
                    && offer.descriptor.address.resource.ends_with(&item)
            })
            .cloned()?;
        let mut arguments = Map::new();
        arguments.insert("conversation".into(), json!(fold.conversation));
        arguments.insert("through".into(), json!(fold.through));
        arguments.insert("through_ts".into(), json!(fold.through_ts));
        // Checked with a text of its own, so a workspace whose verb takes other arguments is
        // found out before a model is asked.
        let mut checked = arguments.clone();
        checked.insert("text".into(), json!(fold.summary));
        match super::task_action_from_offer(&offer, checked) {
            Ok(mut action) => {
                action.arguments = arguments;
                Some(action)
            }
            Err(error) => {
                tracing::warn!(%error, "the conversation's `keep-summary` refused its arguments");
                None
            }
        }
    }

    /// The one answer to what `fold` asks, from the model this session asks single questions of.
    /// `None` where none came: no model, a call that failed or ran out of time, or the person
    /// ending the turn meanwhile, which stops the call.
    fn ask_once(&mut self, run: &Asked, fold: &Fold) -> Option<String> {
        let model = Arc::clone(&self.single_shot);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let (instructions, input) = fold.asks();
        let tokens = fold.output_tokens();
        let (answered, answer) = channel();
        let spawned = std::thread::Builder::new()
            .name("tilo-agent-summary".into())
            .spawn(move || {
                let said = model.answer_until(
                    Interaction::OneShot,
                    (&instructions, &input),
                    tokens,
                    ASK_TIMEOUT,
                    &stopped,
                );
                let _ = answered.send(said);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "the summary's question could not be asked");
            return None;
        }
        loop {
            match answer.recv_timeout(LOOK_EVERY) {
                Ok(Ok(said)) => return Some(said),
                Ok(Err(why)) => {
                    tracing::info!(%why, "no model answered for the conversation's summary");
                    return None;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.task_cancelled(run) {
                        stop.store(true, Ordering::Relaxed);
                        return None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}
