//! The system text-to-speech engine as a [`tts::Speaker`], which the
//! built-in screen reader narrates through.

use jni::JNIEnv;
use jni::objects::{GlobalRef, JObject, JValue};

use super::bridge::{self, SPEECH};

/// The system engine, or a [`tts::NullSpeaker`] when it cannot be reached.
pub(super) fn speaker() -> Box<dyn tts::Speaker> {
    match AndroidSpeaker::new() {
        Ok(speaker) => Box::new(speaker),
        Err(error) => {
            tracing::warn!(target: "tilo_ui::speech", %error, "no android text-to-speech; speech is silent");
            Box::new(tts::NullSpeaker::new())
        }
    }
}

struct AndroidSpeaker {
    engine: GlobalRef,
    /// Whether the engine refused the last utterance, so a broken engine is
    /// reported once rather than per utterance.
    refusing: bool,
}

impl AndroidSpeaker {
    fn new() -> Result<Self, String> {
        let engine = super::with_activity(|env, activity| {
            let class = bridge::class(env, activity, SPEECH)?;
            let engine = env.new_object(
                &class,
                "(Landroid/content/Context;)V",
                &[JValue::Object(activity)],
            )?;
            env.new_global_ref(engine)
        })?;
        Ok(Self {
            engine,
            refusing: false,
        })
    }

    fn call<T>(
        &self,
        f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<T>,
    ) -> Result<T, String> {
        super::with_activity(|env, _| f(env, self.engine.as_obj()))
    }
}

impl tts::Speaker for AndroidSpeaker {
    fn speak_utterance(&mut self, utterance: tts::Utterance) {
        let spoken = self.call(|env, engine| {
            let text = env.new_string(&utterance.text)?;
            env.call_method(
                engine,
                "speak",
                "(Ljava/lang/String;Z)Z",
                &[
                    JValue::Object(&text),
                    JValue::Bool(utterance.interrupt.into()),
                ],
            )?
            .z()
        });
        match spoken {
            Ok(true) => self.refusing = false,
            Ok(false) => {
                if !self.refusing {
                    tracing::warn!(target: "tilo_ui::speech", "the text-to-speech engine refused to speak");
                }
                self.refusing = true;
            }
            Err(error) => {
                tracing::warn!(target: "tilo_ui::speech", %error, "text-to-speech failed");
            }
        }
    }

    fn stop(&mut self) {
        if let Err(error) =
            self.call(|env, engine| env.call_method(engine, "stop", "()V", &[]).map(drop))
        {
            tracing::warn!(target: "tilo_ui::speech", %error, "could not stop text-to-speech");
        }
    }

    fn set_rate(&mut self, rate: f32) {
        // Android's rate is the same multiplier: 1.0 is the engine's normal.
        let rate = if rate.is_finite() && rate > 0.0 {
            rate.clamp(0.25, 4.0)
        } else {
            1.0
        };
        if let Err(error) = self.call(|env, engine| {
            env.call_method(engine, "setRate", "(F)V", &[JValue::Float(rate)])
                .map(drop)
        }) {
            tracing::warn!(target: "tilo_ui::speech", %error, "could not set the speech rate");
        }
    }

    fn set_voice(&mut self, voice: &str) {
        let selected = self.call(|env, engine| {
            let voice = env.new_string(voice)?;
            env.call_method(
                engine,
                "setVoice",
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&voice)],
            )?
            .z()
        });
        if !matches!(selected, Ok(true)) {
            tracing::warn!(target: "tilo_ui::speech", voice, "the text-to-speech engine has no such voice");
        }
    }

    fn is_speaking(&self) -> bool {
        self.call(|env, engine| env.call_method(engine, "isSpeaking", "()Z", &[])?.z())
            .unwrap_or(false)
    }

    fn backend_name(&self) -> &'static str {
        "android-tts"
    }
}

impl Drop for AndroidSpeaker {
    fn drop(&mut self) {
        let _ = self.call(|env, engine| env.call_method(engine, "shutdown", "()V", &[]).map(drop));
    }
}
