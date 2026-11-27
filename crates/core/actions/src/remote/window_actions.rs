//! What every app's window answers beside its declared actions: a link's scheme, the
//! answer that asks the window to come forward, and the item a `render` payload shows first.

/// The scheme of `link`, lowercase, as an app's manifest claims it
/// (`geo` for `geo:52.5,13.4`); `None` for text that names no scheme
/// (a path, a bare word).
#[must_use]
pub fn link_scheme(link: &str) -> Option<String> {
    let (scheme, rest) = link.split_once(':')?;
    let mut chars = scheme.chars();
    // One letter is a drive (`C:\notes`), never a scheme.
    let valid = scheme.len() > 1
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty();
    valid.then(|| scheme.to_ascii_lowercase())
}

/// What an `activate-window` answers once the window was asked to come
/// forward.
#[must_use]
pub fn activation_requested() -> crate::ActionOutput {
    crate::ActionOutput {
        value: Some(serde_json::json!({"activation_requested": true})),
        resources: Vec::new(),
    }
}

/// The item a `tilo.app.render` payload asks to be shown first, when it asks for one: `anchor`,
/// the name the shared `outline` declares for the same argument.
#[must_use]
pub fn render_anchor(payload: &[u8]) -> Option<String> {
    let args: serde_json::Value = serde_json::from_slice(payload).ok()?;
    args.get("anchor")
        .and_then(serde_json::Value::as_str)
        .filter(|anchor| !anchor.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::link_scheme;

    #[test]
    fn a_link_names_its_scheme_and_nothing_else_does() {
        assert_eq!(link_scheme("geo:52.5,13.4").as_deref(), Some("geo"));
        assert_eq!(
            link_scheme("Settings://keyboard").as_deref(),
            Some("settings")
        );
        assert_eq!(
            link_scheme("web+search:cats").as_deref(),
            Some("web+search")
        );
        for none in [
            "/home/a/b.txt",
            "cafe",
            "geo:",
            "C:\\notes",
            "1x:y",
            ":a",
            "ht tp://x",
        ] {
            assert_eq!(link_scheme(none), None, "{none}");
        }
    }
}
