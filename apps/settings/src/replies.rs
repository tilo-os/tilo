//! The callers an effect answers once the request it started finishes.

use std::collections::HashMap;

/// The callers waiting for a model's requests to finish, by effect id.
#[derive(Default)]
pub struct Replies {
    waiting: HashMap<&'static str, ui::framework::EffectReply>,
}

impl Replies {
    /// `reply` waits for the request `effect` started.
    pub fn wait(&mut self, effect: &'static str, reply: ui::framework::EffectReply) {
        self.waiting.insert(effect, reply);
    }

    /// Tell the caller of `effect` it succeeded, saying `message`.
    pub fn succeed(&mut self, effect: &'static str, message: impl Into<String>) {
        self.answer(effect, serde_json::Value::String(message.into()));
    }

    /// Tell the caller of `effect` it succeeded, answering `value`.
    pub fn answer(&mut self, effect: &'static str, value: serde_json::Value) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Ok(ui::tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            }));
        }
    }

    /// Tell the caller of `effect` why it failed.
    pub fn fail(&mut self, effect: &'static str, message: impl Into<String>) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Err(message.into()));
        }
    }

    /// Tell the caller of `effect` how it ended.
    pub fn finish(&mut self, effect: &'static str, result: Result<String, String>) {
        match result {
            Ok(message) => self.succeed(effect, message),
            Err(message) => self.fail(effect, message),
        }
    }
}
