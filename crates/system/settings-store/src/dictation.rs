//! Personal dictation language intent; never a microphone permission.
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

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

/// Only a code of the list reads, or nothing (`null`), which is automatic
/// detection: a field edit with any other value is refused at the field.
/// The section's reader keeps automatic detection for a stored value that
/// does not read (`tilo_accessibility_settings`), so an unknown code
/// written by another version still opens.
impl<'de> Deserialize<'de> for DictationLanguage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_option(LanguageVisitor)
    }
}

struct LanguageVisitor;

impl<'de> Visitor<'de> for LanguageVisitor {
    type Value = DictationLanguage;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a dictation language code")
    }

    fn visit_str<E: de::Error>(self, code: &str) -> Result<Self::Value, E> {
        DictationLanguage::from_code(code)
            .ok_or_else(|| E::invalid_value(de::Unexpected::Str(code), &self))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_some<D: Deserializer<'de>>(self, inner: D) -> Result<Self::Value, D::Error> {
        inner.deserialize_str(self)
    }
}

#[cfg(feature = "model")]
tilo_resource::json_model!(DictationLanguage);

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_round_trip_and_anything_else_is_refused() {
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
            serde_json::json!("Deutsch"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!(["en"]),
            serde_json::json!({ "code": "en" }),
        ] {
            assert!(
                serde_json::from_value::<DictationLanguage>(value.clone()).is_err(),
                "{value}"
            );
        }
        assert_eq!(
            serde_json::from_value::<DictationLanguage>(serde_json::Value::Null).unwrap(),
            DictationLanguage::default()
        );
        assert!(DictationLanguage::from_code("en-US").is_none());
        assert!(DictationLanguage::from_code("../../path").is_none());
    }
}
