//! Personal dictation language intent; never a microphone permission.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A bounded stable language code understood by the local speech protocol.
/// Automatic detection also covers supported languages outside this quick list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DictationLanguage(&'static str);

impl Default for DictationLanguage {
    fn default() -> Self {
        Self("auto")
    }
}

impl<'de> Deserialize<'de> for DictationLanguage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(value.as_str().and_then(Self::from_code).unwrap_or_default())
    }
}

impl DictationLanguage {
    pub const ALL: &'static [Self] = &[
        Self("auto"),
        Self("en"),
        Self("de"),
        Self("fr"),
        Self("es"),
        Self("ar"),
        Self("zh"),
        Self("ja"),
        Self("it"),
        Self("pt"),
        Self("hi"),
        Self("ru"),
        Self("uk"),
        Self("ko"),
        Self("tr"),
        Self("pl"),
        Self("id"),
        Self("vi"),
        Self("nl"),
        Self("bn"),
        Self("he"),
        Self("fa"),
        Self("th"),
        Self("sv"),
        Self("cs"),
        Self("el"),
        Self("ta"),
        Self("ur"),
        Self("hu"),
        Self("ro"),
        Self("fi"),
    ];

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|language| language.0 == code)
            .copied()
    }

    pub const fn code(self) -> &'static str {
        self.0
    }

    pub fn label_key(self) -> String {
        format!("dictation-language-{}", self.0)
    }

    pub fn load() -> Self {
        Self::from_section(&super::load_section("accessibility"))
    }

    pub fn from_section(section: &serde_json::Map<String, Value>) -> Self {
        section
            .get("dictation_language")
            .and_then(Value::as_str)
            .and_then(Self::from_code)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_round_trip_and_unknown_preferences_are_forgiving() {
        for language in DictationLanguage::ALL {
            let value = serde_json::to_value(language).unwrap();
            assert_eq!(value.as_str(), Some(language.code()));
            assert_eq!(
                serde_json::from_value::<DictationLanguage>(value).unwrap(),
                *language
            );
        }
        for value in [
            serde_json::json!("future-language"),
            serde_json::json!(42),
            Value::Null,
        ] {
            assert_eq!(
                serde_json::from_value::<DictationLanguage>(value).unwrap(),
                DictationLanguage::default()
            );
        }
        assert!(DictationLanguage::from_code("en-US").is_none());
        assert!(DictationLanguage::from_code("../../path").is_none());
    }
}
