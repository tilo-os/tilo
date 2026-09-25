// Script-family mapping adapted from cosmic-text 0.19.0 (MIT OR Apache-2.0).
use cosmic_text::Fallback;
use unicode_script::Script;

pub(super) struct TiloFallback;

fn han(locale: &str) -> &'static [&'static str] {
    let normalized = locale.replace('_', "-").to_ascii_lowercase();
    let parts: Vec<_> = normalized.split(['-', '.', '@']).collect();
    match parts.first().copied() {
        Some("ja") => &["Noto Sans CJK JP"],
        Some("ko") => &["Noto Sans CJK KR"],
        Some("zh") if parts.contains(&"hk") => &["Noto Sans CJK HK"],
        Some("zh") if parts.contains(&"tw") || parts.contains(&"hant") => &["Noto Sans CJK TC"],
        _ => &["Noto Sans CJK SC"],
    }
}

impl Fallback for TiloFallback {
    fn common_fallback(&self) -> &[&'static str] {
        &[
            "Noto Sans",
            "Noto Sans Mono",
            "Noto Sans Symbols",
            "Noto Color Emoji",
            "Noto Sans Symbols 2",
        ]
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        &[".LastResort"]
    }

    fn script_fallback(&self, script: Script, locale: &str) -> &[&'static str] {
        match script {
            Script::Adlam => &["Noto Sans Adlam", "Noto Sans Adlam Unjoined"],
            Script::Arabic => &["Noto Sans Arabic"],
            Script::Armenian => &["Noto Sans Armenian"],
            Script::Bengali => &["Noto Sans Bengali"],
            Script::Bopomofo | Script::Han => han(locale),
            Script::Braille => &["FreeMono"],
            Script::Buhid => &["Noto Sans Buhid"],
            Script::Chakma => &["Noto Sans Chakma"],
            Script::Cherokee => &["Noto Sans Cherokee"],
            Script::Deseret => &["Noto Sans Deseret"],
            Script::Devanagari => &["Noto Sans Devanagari"],
            Script::Ethiopic => &["Noto Sans Ethiopic"],
            Script::Georgian => &["Noto Sans Georgian"],
            Script::Gothic => &["Noto Sans Gothic"],
            Script::Grantha => &["Noto Sans Grantha"],
            Script::Gujarati => &["Noto Sans Gujarati"],
            Script::Gurmukhi => &["Noto Sans Gurmukhi"],
            Script::Hangul => &["Noto Sans CJK KR"],
            Script::Hanunoo => &["Noto Sans Hanunoo"],
            Script::Hebrew => &["Noto Sans Hebrew"],
            Script::Hiragana | Script::Katakana => &["Noto Sans CJK JP"],
            Script::Javanese => &["Noto Sans Javanese"],
            Script::Kannada => &["Noto Sans Kannada"],
            Script::Khmer => &["Noto Sans Khmer"],
            Script::Lao => &["Noto Sans Lao"],
            Script::Malayalam => &["Noto Sans Malayalam"],
            Script::Mongolian => &["Noto Sans Mongolian"],
            Script::Myanmar => &["Noto Sans Myanmar"],
            Script::Oriya => &["Noto Sans Oriya"],
            Script::Runic => &["Noto Sans Runic"],
            Script::Sinhala => &["Noto Sans Sinhala"],
            Script::Syriac => &["Noto Sans Syriac"],
            Script::Tagalog => &["Noto Sans Tagalog"],
            Script::Tagbanwa => &["Noto Sans Tagbanwa"],
            Script::Tai_Le => &["Noto Sans Tai Le"],
            Script::Tai_Tham => &["Noto Sans Tai Tham"],
            Script::Tai_Viet => &["Noto Sans Tai Viet"],
            Script::Tamil => &["Noto Sans Tamil"],
            Script::Telugu => &["Noto Sans Telugu"],
            Script::Thaana => &["Noto Sans Thaana"],
            Script::Thai => &["Noto Sans Thai"],
            Script::Tibetan => &["Noto Serif Tibetan"],
            Script::Tifinagh => &["Noto Sans Tifinagh"],
            Script::Vai => &["Noto Sans Vai"],
            Script::Yi => &["Noto Sans Yi", "Noto Sans CJK SC"],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_forms_and_script_overrides_are_stable() {
        for (locale, family) in [
            ("en-US", "SC"),
            ("ja", "JP"),
            ("ja-JP", "JP"),
            ("ja_JP.UTF-8", "JP"),
            ("KO-kr", "KR"),
            ("zh-TW", "TC"),
            ("zh_Hant_TW", "TC"),
            ("zh-Hant", "TC"),
            ("zh-Hant-HK", "HK"),
            ("zh_CN", "SC"),
        ] {
            assert_eq!(han(locale), [format!("Noto Sans CJK {family}")]);
            assert_eq!(
                TiloFallback.script_fallback(Script::Hiragana, locale),
                ["Noto Sans CJK JP"]
            );
            assert_eq!(
                TiloFallback.script_fallback(Script::Hangul, locale),
                ["Noto Sans CJK KR"]
            );
        }
        assert_eq!(
            TiloFallback.common_fallback(),
            [
                "Noto Sans",
                "Noto Sans Mono",
                "Noto Sans Symbols",
                "Noto Color Emoji",
                "Noto Sans Symbols 2"
            ]
        );
        assert_eq!(TiloFallback.forbidden_fallback(), [".LastResort"]);
    }
}
