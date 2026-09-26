//! The content-line layer shared by iCalendar (RFC 5545) and vCard
//! (RFC 6350).
//!
//! Both formats are streams of `[group.]NAME[;PARAM=value…]:value` lines,
//! folded at 75 octets, with the same TEXT escaping and the same parameter
//! quoting (plus RFC 6868's caret encoding). This crate owns that layer so
//! the calendar, contacts and mail code only deal with properties.
//!
//! Parsing is lenient where real-world producers are sloppy (LF-only line
//! ends, lowercase names, bare vCard 2.1 parameters such as `TEL;WORK:`) and
//! exact where ambiguity would corrupt data: a `:` or `;` inside a quoted
//! parameter value never ends the parameter.

use std::borrow::Cow;

/// RFC 5545 §3.1 / RFC 6350 §3.2: physical lines are at most 75 octets,
/// excluding the CRLF.
pub const FOLD_OCTETS: usize = 75;

/// One unfolded (logical) line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unfolded {
    Line(String),
    /// The logical line exceeded the caller's byte limit. Every continuation
    /// of an oversized line is absorbed, so no fragment of it can later be
    /// mistaken for a property.
    Oversized,
}

/// Undo line folding: a physical line starting with SP or HTAB continues the
/// previous one, minus that single whitespace octet. Accepts CRLF or bare LF.
pub fn unfold(text: &str) -> Vec<String> {
    unfold_bounded(text, usize::MAX)
        .into_iter()
        .filter_map(|line| match line {
            Unfolded::Line(line) => Some(line),
            Unfolded::Oversized => None,
        })
        .collect()
}

/// [`unfold`], replacing any logical line longer than `max_line_bytes` with
/// [`Unfolded::Oversized`] instead of accumulating it.
pub fn unfold_bounded(text: &str, max_line_bytes: usize) -> Vec<Unfolded> {
    let mut out: Vec<Unfolded> = Vec::new();
    for physical in text.split('\n') {
        let physical = physical.strip_suffix('\r').unwrap_or(physical);
        let continuation = physical
            .strip_prefix(' ')
            .or_else(|| physical.strip_prefix('\t'));
        if let (Some(rest), Some(last)) = (continuation, out.last_mut()) {
            if let Unfolded::Line(line) = last {
                if line.len().saturating_add(rest.len()) > max_line_bytes {
                    *last = Unfolded::Oversized;
                } else {
                    line.push_str(rest);
                }
            }
            continue;
        }
        out.push(if physical.len() > max_line_bytes {
            Unfolded::Oversized
        } else {
            Unfolded::Line(physical.to_owned())
        });
    }
    out
}

/// Unfold a document that must use CRLF line ends throughout (as MIME
/// transport of `text/calendar` requires), parsing every logical line.
/// Returns `None` for a bare CR or LF, a missing final CRLF, or a line that
/// is not a content line. Blank lines are not content lines either.
pub fn parse_strict(text: &str) -> Option<Vec<ContentLine>> {
    let body = text.strip_suffix("\r\n")?;
    let mut logical: Vec<String> = Vec::new();
    for physical in body.split("\r\n") {
        if physical.contains(['\r', '\n']) {
            return None;
        }
        let continuation = physical
            .strip_prefix(' ')
            .or_else(|| physical.strip_prefix('\t'));
        match (continuation, logical.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            (Some(_), None) => return None,
            (None, _) => logical.push(physical.to_owned()),
        }
    }
    logical
        .iter()
        .map(|line| ContentLine::parse(line))
        .collect()
}

/// Append `line` folded to [`FOLD_OCTETS`]-octet physical lines, each ended
/// by CRLF. Continuations start with one space, which counts towards their
/// budget, and folds fall only on UTF-8 character boundaries.
pub fn fold_into(out: &mut String, line: &str) {
    let mut used = 0_usize;
    let mut continuation = false;
    for ch in line.chars() {
        let width = ch.len_utf8();
        let budget = if continuation {
            FOLD_OCTETS.saturating_sub(1)
        } else {
            FOLD_OCTETS
        };
        if used > 0 && used.saturating_add(width) > budget {
            out.push_str("\r\n ");
            continuation = true;
            used = 0;
        }
        out.push(ch);
        used = used.saturating_add(width);
    }
    out.push_str("\r\n");
}

/// Append one property: `name`, then each parameter with its value quoted
/// and caret-encoded as needed, then `:` and `value` verbatim. The caller
/// escapes `value` for its type (see [`escape_text`]).
pub fn write_property(out: &mut String, name: &str, params: &[(&str, &str)], value: &str) {
    let mut line = String::with_capacity(name.len().saturating_add(value.len()).saturating_add(1));
    line.push_str(name);
    for (param, param_value) in params {
        line.push(';');
        line.push_str(param);
        line.push('=');
        line.push_str(&encode_param_value(param_value));
    }
    line.push(':');
    line.push_str(value);
    fold_into(out, &line);
}

/// Encode a parameter value: RFC 6868 caret encoding for `^`, newline and
/// `"`, other control characters dropped, and DQUOTE quoting when the value
/// contains `,`, `;` or `:`.
pub fn encode_param_value(value: &str) -> Cow<'_, str> {
    let plain = !value
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '^' | '"' | ',' | ';' | ':'));
    if plain {
        return Cow::Borrowed(value);
    }
    let mut encoded = String::with_capacity(value.len().saturating_add(2));
    for ch in value.chars() {
        match ch {
            '^' => encoded.push_str("^^"),
            '\n' => encoded.push_str("^n"),
            '"' => encoded.push_str("^'"),
            '\t' => encoded.push('\t'),
            ch if ch.is_control() => {}
            ch => encoded.push(ch),
        }
    }
    if encoded.contains([',', ';', ':']) {
        Cow::Owned(format!("\"{encoded}\""))
    } else {
        Cow::Owned(encoded)
    }
}

/// Decode RFC 6868 caret escapes: `^n`/`^N` → newline, `^'` → `"`, `^^` →
/// `^`. Any other caret is literal.
pub fn decode_param_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '^' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('n' | 'N') => {
                out.push('\n');
                chars.next();
            }
            Some('\'') => {
                out.push('"');
                chars.next();
            }
            Some('^') => {
                out.push('^');
                chars.next();
            }
            _ => out.push('^'),
        }
    }
    out
}

/// Escape a TEXT value: backslash, newline, `,` and `;`. Carriage returns
/// are dropped, so CRLF-separated input becomes one escaped newline.
pub fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            other => out.push(other),
        }
    }
    out
}

/// Resolve TEXT escapes: `\n`/`\N` → newline and `\x` → `x`. A trailing lone
/// backslash is dropped.
pub fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Split a structured value (`N`, `ADR`, `CATEGORIES`, …) on `delimiter`
/// where it is not backslash-escaped. Segments keep their escapes for
/// [`unescape_text`].
pub fn split_unescaped(value: &str, delimiter: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0_usize;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if ch == delimiter && !escaped {
            out.push(value.get(start..index).unwrap_or_default());
            start = index.saturating_add(ch.len_utf8());
            escaped = false;
            continue;
        }
        escaped = ch == '\\' && !escaped;
    }
    out.push(value.get(start..).unwrap_or_default());
    out
}

/// One parameter. Names are upper-cased; values are unquoted and
/// caret-decoded. A bare vCard 2.1 parameter (`TEL;WORK:`) has no values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub values: Vec<String>,
}

/// One parsed logical line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentLine {
    /// vCard group (`item1` in `item1.TEL`), as written.
    pub group: Option<String>,
    /// Upper-cased property name.
    pub name: String,
    pub params: Vec<Param>,
    /// The raw value after the first unquoted `:`, still escaped.
    pub value: String,
}

impl ContentLine {
    /// Parse one logical line; `None` if it has no name or no value
    /// separator, or leaves a quoted parameter value open.
    pub fn parse(line: &str) -> Option<Self> {
        let bytes = line.as_bytes();
        let name_end = scan(bytes, 0, |byte| matches!(byte, b';' | b':'))?;
        let full_name = line.get(..name_end)?.trim();
        let (group, name) = match full_name.rsplit_once('.') {
            Some((group, name)) => (Some(group.to_owned()), name),
            None => (None, full_name),
        };
        if name.is_empty() {
            return None;
        }
        let mut pos = name_end;
        let mut params = Vec::new();
        while bytes.get(pos) == Some(&b';') {
            let start = pos.checked_add(1)?;
            let end = scan(bytes, start, |byte| matches!(byte, b'=' | b';' | b':'))?;
            let param_name = line.get(start..end)?.trim().to_ascii_uppercase();
            pos = end;
            let mut values = Vec::new();
            if bytes.get(pos) == Some(&b'=') {
                loop {
                    let start = pos.checked_add(1)?;
                    if bytes.get(start) == Some(&b'"') {
                        let open = start.checked_add(1)?;
                        let close = scan(bytes, open, |byte| byte == b'"')?;
                        values.push(decode_param_value(line.get(open..close)?));
                        pos = close.checked_add(1)?;
                    } else {
                        let end = scan(bytes, start, |byte| matches!(byte, b',' | b';' | b':'))?;
                        values.push(decode_param_value(line.get(start..end)?));
                        pos = end;
                    }
                    if bytes.get(pos) != Some(&b',') {
                        break;
                    }
                }
            }
            if !param_name.is_empty() {
                params.push(Param {
                    name: param_name,
                    values,
                });
            }
        }
        if bytes.get(pos) != Some(&b':') {
            return None;
        }
        Some(Self {
            group,
            name: name.to_ascii_uppercase(),
            params,
            value: line.get(pos.checked_add(1)?..)?.to_owned(),
        })
    }

    /// The first parameter named `name` (case-insensitive).
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params
            .iter()
            .find(|param| param.name.eq_ignore_ascii_case(name))
    }

    /// A parameter's values joined by `,`, as a single-valued parameter reads.
    pub fn param_value(&self, name: &str) -> Option<String> {
        self.param(name).map(|param| param.values.join(","))
    }

    /// Whether this is `BEGIN:<component>` (case-insensitive).
    pub fn begins(&self, component: &str) -> bool {
        self.name == "BEGIN" && self.value.trim().eq_ignore_ascii_case(component)
    }

    /// Whether this is `END:<component>` (case-insensitive).
    pub fn ends(&self, component: &str) -> bool {
        self.name == "END" && self.value.trim().eq_ignore_ascii_case(component)
    }
}

fn scan(bytes: &[u8], from: usize, stop: impl Fn(u8) -> bool) -> Option<usize> {
    let offset = bytes.get(from..)?.iter().position(|byte| stop(*byte))?;
    from.checked_add(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folded(line: &str) -> String {
        let mut out = String::new();
        fold_into(&mut out, line);
        out
    }

    #[test]
    fn folding_and_unfolding_respect_octet_budgets_and_continuations() {
        // Phase `folding_respects_the_octet_budget_and_utf8_boundaries`.
        {
            for line in [
                format!("NOTE:{}", "x".repeat(200)),
                format!("SUMMARY:Planning {}", "界".repeat(40)),
                format!("NOTE:{}", "é".repeat(80)),
                format!("NOTE:{}", "🎉".repeat(50)),
            ] {
                let out = folded(&line);
                assert!(out.ends_with("\r\n"));
                let physical: Vec<&str> = out.trim_end_matches("\r\n").split("\r\n").collect();
                assert!(physical.len() > 1, "long line must fold");
                for (index, part) in physical.iter().enumerate() {
                    assert!(part.len() <= FOLD_OCTETS, "{} octets: {part:?}", part.len());
                    assert_eq!(index > 0, part.starts_with(' '));
                }
                assert_eq!(unfold(&out), vec![line.clone(), String::new()]);
            }
        }
        // Phase `folding_fills_each_line_exactly`.
        {
            let out = folded(&"a".repeat(75 + 74 + 1));
            assert_eq!(
                out,
                format!("{}\r\n {}\r\n a\r\n", "a".repeat(75), "a".repeat(74))
            );
            assert_eq!(folded("SHORT:x"), "SHORT:x\r\n");
        }
        // Phase `unfolding_accepts_lf_tab_and_leading_continuations`.
        {
            let text = "SUMMARY:Hello Wo\r\n rld\nNOTE:a\n\tb\r\n";
            assert_eq!(unfold(text), vec!["SUMMARY:Hello World", "NOTE:ab", ""]);
            assert_eq!(unfold(" orphan\r\n"), vec![" orphan", ""]);
        }
        // Phase `bounded_unfolding_absorbs_every_continuation_of_an_oversized_line`.
        {
            let text = format!(
                "FN:ok\r\nNOTE:{}\r\n more\r\n NOTE:smuggled\r\nEND:VCARD\r\n",
                "x".repeat(20)
            );
            let lines = unfold_bounded(&text, 16);
            assert_eq!(
                lines,
                vec![
                    Unfolded::Line("FN:ok".into()),
                    Unfolded::Oversized,
                    Unfolded::Line("END:VCARD".into()),
                    Unfolded::Line(String::new()),
                ]
            );
            let accumulated = unfold_bounded("NOTE:0123456789\r\n 0123456789\r\n", 16);
            assert_eq!(accumulated[0], Unfolded::Oversized);
        }
    }

    #[test]
    fn content_lines_parse_and_round_trip_params_escapes_and_structure() {
        // Phase `content_lines_parse_groups_params_and_values`.
        {
            let line = ContentLine::parse("item1.tel;type=WORK,voice;PREF=1:+1 555").unwrap();
            assert_eq!(line.group.as_deref(), Some("item1"));
            assert_eq!(line.name, "TEL");
            assert_eq!(line.params[0].name, "TYPE");
            assert_eq!(line.params[0].values, vec!["WORK", "voice"]);
            assert_eq!(line.param_value("pref").as_deref(), Some("1"));
            assert_eq!(line.value, "+1 555");

            let bare = ContentLine::parse("TEL;WORK;HOME:1").unwrap();
            assert_eq!(bare.params.len(), 2);
            assert!(bare.params.iter().all(|param| param.values.is_empty()));
        }
        // Phase `quoted_parameters_may_contain_delimiters`.
        {
            let line = ContentLine::parse(
                "ATTENDEE;CN=\"Doe, Jane; PhD\";DELEGATED-FROM=\"mailto:a@example.com\":mailto:jane@example.com",
            )
            .unwrap();
            assert_eq!(line.param_value("CN").as_deref(), Some("Doe, Jane; PhD"));
            assert_eq!(
                line.param_value("DELEGATED-FROM").as_deref(),
                Some("mailto:a@example.com")
            );
            assert_eq!(line.value, "mailto:jane@example.com");

            let list = ContentLine::parse("TEL;TYPE=\"voice,home\",\"cell\":x").unwrap();
            assert_eq!(list.params[0].values, vec!["voice,home", "cell"]);

            assert!(ContentLine::parse("X;CN=\"open:value").is_none());
            assert!(ContentLine::parse("no separator").is_none());
            assert!(ContentLine::parse(":value").is_none());
        }
        // Phase `parameter_values_round_trip_through_quoting_and_caret_encoding`.
        {
            for value in [
                "Sam Example",
                "Doe, Jane",
                "a;b:c",
                "say \"hi\"",
                "two\nlines",
                "caret ^ here",
                "Europe/Berlin",
            ] {
                let mut out = String::new();
                write_property(
                    &mut out,
                    "ATTENDEE",
                    &[("CN", value)],
                    "mailto:x@example.com",
                );
                let logical = unfold(&out);
                let line = ContentLine::parse(&logical[0]).unwrap();
                assert_eq!(line.param_value("CN").as_deref(), Some(value), "{out}");
                assert_eq!(line.value, "mailto:x@example.com");
            }
            assert!(matches!(
                encode_param_value("Sam Example"),
                Cow::Borrowed(_)
            ));
            assert_eq!(encode_param_value("Doe, Jane"), "\"Doe, Jane\"");
            assert_eq!(decode_param_value("^x^"), "^x^");
        }
        // Phase `text_escaping_round_trips_and_normalises_crlf`.
        {
            let text = "Note: with, punctuation; back\\slash\nnewline";
            let escaped = escape_text(text);
            assert_eq!(
                escaped,
                "Note: with\\, punctuation\\; back\\\\slash\\nnewline"
            );
            assert_eq!(unescape_text(&escaped), text);
            assert_eq!(escape_text("a\r\nb"), "a\\nb");
            assert_eq!(unescape_text("A\\NB\\"), "A\nB");
        }
        // Phase `structured_values_split_only_on_unescaped_delimiters`.
        {
            assert_eq!(
                split_unescaped("Doe\\;Smith;Jane;;;", ';'),
                vec!["Doe\\;Smith", "Jane", "", "", ""]
            );
            assert_eq!(split_unescaped("a\\\\,b", ','), vec!["a\\\\", "b"]);
            assert_eq!(split_unescaped("", ','), vec![""]);
        }
        // Phase `strict_documents_require_crlf_and_valid_lines`.
        {
            let ok = "BEGIN:VCALENDAR\r\nSUMMARY:a\r\n b\r\nEND:VCALENDAR\r\n";
            let lines = parse_strict(ok).unwrap();
            assert!(lines[0].begins("vcalendar"));
            assert_eq!(lines[1].value, "ab");
            assert!(lines[2].ends("VCALENDAR"));
            for bad in [
                "BEGIN:VCALENDAR\nEND:VCALENDAR\n",
                "BEGIN:VCALENDAR\r\nEND:VCALENDAR",
                "BEGIN:VCALENDAR\r\n\r\nEND:VCALENDAR\r\n",
                " BEGIN:VCALENDAR\r\n",
                "BEGIN:VCALENDAR\r\nSUMMARY:a\rb\r\n",
            ] {
                assert!(parse_strict(bad).is_none(), "{bad:?}");
            }
        }
    }
}
