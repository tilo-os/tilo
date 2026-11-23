//! Minimal RFC-4180-ish CSV import/export, and the same for a table whose
//! fields are separated by tabs (TSV).
//!
//! Import maps each field to a raw cell input at `(col, row)` so formulas
//! survive a round-trip if they were written verbatim. Export renders the
//! computed/display values (what the user sees) so the file is useful in
//! other tools.

use std::collections::HashMap;

use crate::model::Addr;

/// A table's cells as typed, its fields parted by `separator` (`','` for CSV, `'\t'` for TSV).
pub fn parse_separated(text: &str, separator: char) -> HashMap<Addr, String> {
    let mut out = HashMap::new();
    let mut row = 0u32;
    let mut col = 0u32;
    let mut field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let commit_field =
        |field: &mut String, row: u32, col: u32, out: &mut HashMap<Addr, String>, started: bool| {
            let trimmed = field.clone();
            if started && !trimmed.is_empty() {
                out.insert((col, row), trimmed);
            }
            field.clear();
        };

    while let Some(&c) = chars.get(i) {
        if in_quotes {
            if c == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    field.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
                continue;
            }
            field.push(c);
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                field_started = true;
            }
            c if c == separator => {
                commit_field(&mut field, row, col, &mut out, field_started);
                col += 1;
                field_started = false;
            }
            '\r' => { /* swallow; handled by \n */ }
            '\n' => {
                commit_field(&mut field, row, col, &mut out, field_started);
                row += 1;
                col = 0;
                field_started = false;
            }
            _ => {
                field.push(c);
                field_started = true;
            }
        }
        i += 1;
    }
    commit_field(&mut field, row, col, &mut out, field_started);
    out
}

fn quote_field(s: &str, separator: char) -> String {
    if s.contains([separator, '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Rows of texts as one table, its fields parted by `separator` (`','` for CSV, `'\t'` for TSV).
pub fn to_separated(rows: &[Vec<String>], separator: char) -> String {
    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row.iter().map(|f| quote_field(f, separator)).collect();
        out.push_str(&line.join(&separator.to_string()));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_parses_quotes_and_round_trips_escapes() {
        {
            let m = parse_separated("a,b,c\n1,2,3", ',');
            assert_eq!(m.get(&(0, 0)).unwrap(), "a");
            assert_eq!(m.get(&(2, 0)).unwrap(), "c");
            assert_eq!(m.get(&(1, 1)).unwrap(), "2");
        }

        {
            let m = parse_separated("\"hello, world\",x", ',');
            assert_eq!(m.get(&(0, 0)).unwrap(), "hello, world");
            assert_eq!(m.get(&(1, 0)).unwrap(), "x");
        }

        {
            let rows = vec![vec!["a".to_string(), "b,c".to_string()]];
            assert_eq!(to_separated(&rows, ','), "a,\"b,c\"\n");
        }

        {
            let m = parse_separated("\"say \"\"hi\"\"\"", ',');
            assert_eq!(m.get(&(0, 0)).unwrap(), "say \"hi\"");
        }
    }
}
