//! Personal dictation language intent; never a microphone permission.
use serde::de::{self, IgnoredAny, Visitor};
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

/// Any value reads: a code outside the list, or anything that is not a
/// string, is automatic detection. Skipping a non-string rather than building
/// it as a JSON value keeps each format's reader of this small.
impl<'de> Deserialize<'de> for DictationLanguage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LanguageVisitor)
    }
}

struct LanguageVisitor;

impl<'de> Visitor<'de> for LanguageVisitor {
    type Value = DictationLanguage;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a dictation language code")
    }

    fn visit_str<E: de::Error>(self, code: &str) -> Result<Self::Value, E> {
        Ok(DictationLanguage::from_code(code).unwrap_or_default())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(DictationLanguage::default())
    }

    fn visit_some<D: Deserializer<'de>>(self, inner: D) -> Result<Self::Value, D::Error> {
        inner.deserialize_any(self)
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut items: A) -> Result<Self::Value, A::Error> {
        while items.next_element::<IgnoredAny>()?.is_some() {}
        Ok(DictationLanguage::default())
    }

    fn visit_map<A: de::MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
        while entries.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(DictationLanguage::default())
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
            serde_json::json!(-1.5),
            serde_json::json!(true),
            serde_json::json!(["en"]),
            serde_json::json!({ "code": "en" }),
            serde_json::Value::Null,
        ] {
            assert_eq!(
                serde_json::from_value::<DictationLanguage>(value).unwrap(),
                DictationLanguage::default()
            );
        }
        assert_eq!(
            serde_json::from_str::<DictationLanguage>(r#"{"code": ["en", {"a": 1}]}"#).unwrap(),
            DictationLanguage::default()
        );
        assert_eq!(
            serde_json::from_str::<DictationLanguage>(r#""de""#)
                .unwrap()
                .code(),
            "de"
        );
        assert!(DictationLanguage::from_code("en-US").is_none());
        assert!(DictationLanguage::from_code("../../path").is_none());
    }
}
