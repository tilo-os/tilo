//! Conservative final-text edits. Never an instruction-following rewrite.
use std::borrow::Cow;

const ENGLISH_NUMBERS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
];

const GERMAN_NUMBERS: &[&str] = &[
    "null",
    "eins",
    "ein",
    "eine",
    "einen",
    "zwei",
    "drei",
    "vier",
    "fünf",
    "sechs",
    "sieben",
    "acht",
    "neun",
    "zehn",
    "elf",
    "zwölf",
    "dreizehn",
    "vierzehn",
    "fünfzehn",
    "sechzehn",
    "siebzehn",
    "achtzehn",
    "neunzehn",
    "zwanzig",
];

const FRENCH_NUMBERS: &[&str] = &[
    "zéro", "un", "une", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix",
    "onze", "douze", "treize", "quatorze", "quinze", "seize", "dix-sept", "dix-huit", "dix-neuf",
    "vingt",
];

const SPANISH_NUMBERS: &[&str] = &[
    "cero",
    "uno",
    "una",
    "un",
    "dos",
    "tres",
    "cuatro",
    "cinco",
    "seis",
    "siete",
    "ocho",
    "nueve",
    "diez",
    "once",
    "doce",
    "trece",
    "catorce",
    "quince",
    "dieciséis",
    "diecisiete",
    "dieciocho",
    "diecinueve",
    "veinte",
];

/// Remove only explicit, comma-delimited numeric self-repairs in supported languages.
/// Literal mode, unsupported languages, quotations and oversized input are
/// returned byte-for-byte. The replacement's spelling is never normalised.
/// This deliberately does not guess grammar, remove isolated hesitation words,
/// rewrite proper names or infer an unspoken numeric correction.
pub fn clean_final<'a>(text: &'a str, language: &str, literal: bool) -> Cow<'a, str> {
    let Some(profile) = Profile::for_language(&tilo_paths::language_of(language)) else {
        return Cow::Borrowed(text);
    };
    if literal
        || text.len() > super::MAX_TEXT_BYTES
        || text.contains('\u{0027}')
        || text.contains(['"', '“', '”', '‘', '’', '«', '»', '„', '‹', '›'])
    {
        return Cow::Borrowed(text);
    }
    let mut tokens = Vec::new();
    let mut offset = 0;
    for token in text.split_whitespace() {
        let Some(relative) = text.get(offset..).and_then(|tail| tail.find(token)) else {
            return Cow::Borrowed(text);
        };
        let start = offset.saturating_add(relative);
        tokens.push((start, token));
        offset = start.saturating_add(token.len());
    }
    let mut output = String::new();
    let mut copied = 0;
    let mut index = 0;
    while let Some(&(start, token)) = tokens.get(index) {
        if !token
            .strip_suffix(',')
            .is_some_and(|word| profile.number(word))
        {
            index = index.saturating_add(1);
            continue;
        }
        // Uppercase number-like proper names and padded digits are excluded.
        let mut next = index.saturating_add(1);
        if tokens
            .get(next)
            .is_some_and(|(_, word)| profile.fillers.contains(word))
        {
            next = next.saturating_add(1);
        }
        if tokens
            .get(next)
            .is_none_or(|(_, word)| *word != profile.marker)
        {
            index = index.saturating_add(1);
            continue;
        }
        next = next.saturating_add(1);
        let Some(&(replacement, word)) = tokens.get(next) else {
            break;
        };
        if !profile.number(word.trim_end_matches(['.', ',', '!', '?', ';', ':'])) {
            index = index.saturating_add(1);
            continue;
        }
        let Some(prefix) = text.get(copied..start) else {
            return Cow::Borrowed(text);
        };
        output.push_str(prefix);
        copied = replacement;
        // Revisit the replacement: an explicit chain must keep the final
        // correction rather than leaving a stale intermediate number.
        index = next;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    if let Some(tail) = text.get(copied..) {
        output.push_str(tail);
    }
    Cow::Owned(output)
}

struct Profile {
    marker: &'static str,
    fillers: &'static [&'static str],
    numbers: &'static [&'static str],
}

impl Profile {
    fn for_language(language: &str) -> Option<Self> {
        let (marker, fillers, numbers): (_, &[_], &[_]) = match language {
            "en" | "auto" => ("no,", &["uh,", "um,"], ENGLISH_NUMBERS),
            "de" => ("nein,", &["äh,", "ähm,"], GERMAN_NUMBERS),
            "fr" => ("non,", &["euh,"], FRENCH_NUMBERS),
            "es" => ("no,", &["eh,", "em,"], SPANISH_NUMBERS),
            _ => return None,
        };
        Some(Self {
            marker,
            fillers,
            numbers,
        })
    }

    fn number(&self, word: &str) -> bool {
        self.numbers.contains(&word)
            || (!word.is_empty()
                && word.len() <= 6
                && !word.starts_with('0')
                && word.bytes().all(|byte| byte.is_ascii_digit()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One call of `clean_final` a row: the language, whether literal mode is on, what was said and
    /// what is kept. The three groups are the three tests this table was.
    const FINAL_CLEANUP: &[(&str, bool, &str, &str)] = &[
        // explicit_numeric_repair_keeps_replacement_and_surrounding_text
        (
            "en",
            false,
            "Let me grab two, uh, no, three of these",
            "Let me grab three of these",
        ),
        (
            "auto",
            false,
            "Let me grab two, uh, no, three of these",
            "Let me grab three of these",
        ),
        (
            "en",
            true,
            "Let me grab two, uh, no, three of these",
            "Let me grab two, uh, no, three of these",
        ),
        ("en", false, "Take 2, um, no, 3.", "Take 3."),
        ("auto", false, "Take 2, um, no, 3.", "Take 3."),
        ("en", true, "Take 2, um, no, 3.", "Take 2, um, no, 3."),
        (
            "en",
            false,
            "Take two, no, three; then four, uh, no, five.",
            "Take three; then five.",
        ),
        (
            "auto",
            false,
            "Take two, no, three; then four, uh, no, five.",
            "Take three; then five.",
        ),
        (
            "en",
            true,
            "Take two, no, three; then four, uh, no, five.",
            "Take two, no, three; then four, uh, no, five.",
        ),
        (
            "en",
            false,
            "Tilo needs two, uh, no, three.",
            "Tilo needs three.",
        ),
        (
            "auto",
            false,
            "Tilo needs two, uh, no, three.",
            "Tilo needs three.",
        ),
        (
            "en",
            true,
            "Tilo needs two, uh, no, three.",
            "Tilo needs two, uh, no, three.",
        ),
        // selected_languages_and_chained_repairs_preserve_literal_choice
        (
            "en-GB",
            false,
            "Take two, no, three, uh, no, four.",
            "Take four.",
        ),
        (
            "en-GB",
            true,
            "Take two, no, three, uh, no, four.",
            "Take two, no, three, uh, no, four.",
        ),
        (
            "de-DE",
            false,
            "TiLo braucht zwei, äh, nein, drei Stück.",
            "TiLo braucht drei Stück.",
        ),
        (
            "de-DE",
            true,
            "TiLo braucht zwei, äh, nein, drei Stück.",
            "TiLo braucht zwei, äh, nein, drei Stück.",
        ),
        ("de", false, "Nimm 2, ähm, nein, 3, nein, 4.", "Nimm 4."),
        (
            "de",
            true,
            "Nimm 2, ähm, nein, 3, nein, 4.",
            "Nimm 2, ähm, nein, 3, nein, 4.",
        ),
        (
            "fr-FR",
            false,
            "Prendre deux, euh, non, trois exemplaires.",
            "Prendre trois exemplaires.",
        ),
        (
            "fr-FR",
            true,
            "Prendre deux, euh, non, trois exemplaires.",
            "Prendre deux, euh, non, trois exemplaires.",
        ),
        (
            "es-419",
            false,
            "TiLo necesita dos, eh, no, tres unidades.",
            "TiLo necesita tres unidades.",
        ),
        (
            "es-419",
            true,
            "TiLo necesita dos, eh, no, tres unidades.",
            "TiLo necesita dos, eh, no, tres unidades.",
        ),
        (
            "de",
            false,
            "Zwei, äh, nein, Drei ist ein Name.",
            "Zwei, äh, nein, Drei ist ein Name.",
        ),
        ("de", false, "zwei, nein, dreifach", "zwei, nein, dreifach"),
        (
            "fr",
            false,
            "deux, non, trois-quarts",
            "deux, non, trois-quarts",
        ),
        ("es", false, "dos, no, trescientos", "dos, no, trescientos"),
        ("de", false, "zwei, äh, drei", "zwei, äh, drei"),
        ("fr", false, "deux, non, 03", "deux, non, 03"),
        ("es", false, "dos, no, 3.5", "dos, no, 3.5"),
        ("de", false, "«zwei, nein, drei»", "«zwei, nein, drei»"),
        ("auto", false, "zwei, nein, drei", "zwei, nein, drei"),
        ("de", false, "two, no, three", "two, no, three"),
        // uncertain_disfluency_names_quotes_languages_and_numbers_stay_literal
        (
            "en",
            false,
            "Let me, uh, grab three of these",
            "Let me, uh, grab three of these",
        ),
        (
            "en",
            false,
            "Say 'Let me grab two, uh, no, three of these' exactly",
            "Say 'Let me grab two, uh, no, three of these' exactly",
        ),
        (
            "en",
            false,
            "Two, uh, no, Three knows Tilo",
            "Two, uh, no, Three knows Tilo",
        ),
        (
            "en",
            false,
            "Say \"two, uh, no, three\" exactly",
            "Say \"two, uh, no, three\" exactly",
        ),
        ("en", false, "two, no, threefold", "two, no, threefold"),
        ("en", false, "two, no, 03", "two, no, 03"),
        ("en", false, "two, no three", "two, no three"),
        ("en", false, "two uh no three", "two uh no three"),
        ("en", false, "two, no, 3.5", "two, no, 3.5"),
        (
            "en",
            false,
            "two, no, three-legged",
            "two, no, three-legged",
        ),
        ("en", false, "three and two", "three and two"),
        ("en", false, "no, three", "no, three"),
        ("en", false, "two, no, Tilo", "two, no, Tilo"),
        ("fr", false, "two, uh, no, three", "two, uh, no, three"),
        ("de", false, "two, uh, no, three", "two, uh, no, three"),
        ("es", false, "two, uh, no, three", "two, uh, no, three"),
        ("ar", false, "two, uh, no, three", "two, uh, no, three"),
        ("zh", false, "two, uh, no, three", "two, uh, no, three"),
    ];

    #[test]
    fn final_cleanup_removes_only_an_explicit_numeric_repair_in_a_supported_language() {
        for (row, &(language, literal, said, kept)) in FINAL_CLEANUP.iter().enumerate() {
            assert_eq!(
                clean_final(said, language, literal),
                kept,
                "row {row}: {language}, literal {literal}: {said}"
            );
        }
    }
}
