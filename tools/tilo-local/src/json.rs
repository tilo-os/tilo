//! The one piece of JSON this reads: a string field of the flat object the development session
//! writes where it runs (`session.json`: control, runtime, state, bin).

/// The string `key` holds in `text`, a flat JSON object.
pub fn string_field(text: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    let (_, after) = text.split_once(&quoted)?;
    let value = after.trim_start().strip_prefix(':')?.trim_start();
    let mut chars = value.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'u' => {
                    let code: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&code, 16).ok()?)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::string_field;

    #[test]
    fn reads_the_fields_the_session_writes() {
        let text = "{\n  \"control\": \"/run/a b/control.sock\",\n  \"runtime\": \"/r\\\"x\",\n  \"bin\": \"/b\"\n}";
        assert_eq!(
            string_field(text, "control").as_deref(),
            Some("/run/a b/control.sock")
        );
        assert_eq!(string_field(text, "runtime").as_deref(), Some("/r\"x"));
        assert_eq!(string_field(text, "bin").as_deref(), Some("/b"));
        assert_eq!(string_field(text, "state"), None);
    }
}
