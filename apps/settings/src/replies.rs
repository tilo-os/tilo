//! The callers an effect answers once the request it started finishes.

use std::collections::HashMap;

#[derive(Default)]
pub struct Replies {
    waiting: HashMap<&'static str, ui::framework::EffectReply>,
}

impl Replies {
    pub fn wait(&mut self, effect: &'static str, reply: ui::framework::EffectReply) {
        self.waiting.insert(effect, reply);
    }

    pub fn succeed(&mut self, effect: &'static str, message: impl Into<String>) {
        self.answer(effect, serde_json::Value::String(message.into()));
    }

    pub fn answer(&mut self, effect: &'static str, value: serde_json::Value) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Ok(ui::tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            }));
        }
    }

    pub fn fail(&mut self, effect: &'static str, message: impl Into<String>) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Err(message.into()));
        }
    }

    pub fn finish(&mut self, effect: &'static str, result: Result<String, String>) {
        match result {
            Ok(message) => self.succeed(effect, message),
            Err(message) => self.fail(effect, message),
        }
    }
}
