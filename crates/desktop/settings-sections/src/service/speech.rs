//! Whether this device can speak: a screen reader needs a speech engine installed beside Tilo,
//! and Settings says so where it has none.

use serde::{Deserialize, Serialize};

/// The resource the answer is published as.
pub const RESOURCE: &str = "settings/speech";
/// Its kind, as the reading's type is named and a view reads `settings.speech`.
pub const KIND: &str = "settings.speech";

/// Whether a speech engine is installed here, and which one.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Speech {
    /// A speech engine Tilo can read the screen through.
    pub available: bool,
    /// The engine found, by its program's name; empty where none is.
    pub engine: String,
}

impl Speech {
    /// What this device can speak through now.
    #[must_use]
    pub fn reads() -> Self {
        let engine = engine().unwrap_or_default();
        Self {
            available: !engine.is_empty(),
            engine,
        }
    }
}

/// The speech engines a screen reader runs: the ones Tilo reads screens through.
const ENGINES: [&str; 3] = ["speech-dispatcher", "spd-say", "espeak-ng"];

/// The first engine on this machine's path.
fn engine() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for name in ENGINES {
            if directory.join(name).is_file() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// The answer as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.speech", label = "Speech")]
pub struct SpeechReading;

/// The answer as a view of another language reads it (`settings.speech`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Speech, tilo_resource::NoEffect>(KIND, "Speech")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reading says what it found: an engine is named, or none is and the reading says the
    /// screen cannot speak.
    #[test]
    fn the_reading_names_the_engine_it_found_or_none() {
        let speech = Speech::reads();
        assert_eq!(speech.available, !speech.engine.is_empty());
        assert!(
            speech.engine.is_empty() || ENGINES.contains(&speech.engine.as_str()),
            "{} is one of the engines Tilo reads through",
            speech.engine
        );
    }
}
