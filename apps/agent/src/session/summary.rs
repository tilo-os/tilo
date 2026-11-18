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

use super::conversation::MESSAGES;
use super::{Asked, Session};

/// The chat action that keeps a conversation's summary.
const KEEP_SUMMARY: &str = "keep-summary";
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
        let Some((_, message)) = run.message.rsplit_once(MESSAGES) else {
            return Ok(());
        };
        let item = format!("{MESSAGES}{message}");
        // Where the answer could not be kept, no model is asked.
        if self.offered_on(&item, KEEP_SUMMARY).is_none() {
            tracing::info!(
                conversation = %fold.conversation,
                "the workspace offers no `keep-summary` on the turn: the summary stays as it is"
            );
            return Ok(());
        }
        let Some(answer) = self.ask_once(run, &fold) else {
            return Ok(());
        };
        let mut arguments = Map::new();
        arguments.insert("conversation".into(), json!(fold.conversation));
        arguments.insert("through".into(), json!(fold.through));
        arguments.insert("through_ts".into(), json!(fold.through_ts));
        arguments.insert("text".into(), json!(fold.kept(&answer)));
        self.invoke_on_message(run, &item, KEEP_SUMMARY, arguments, true)?;
        tracing::info!(
            conversation = %fold.conversation,
            through = %fold.through,
            rewritten = fold.condenses(),
            "kept the conversation's summary"
        );
        Ok(())
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
                    if self.link.cancelled(run.id) {
                        stop.store(true, Ordering::Relaxed);
                        return None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}
