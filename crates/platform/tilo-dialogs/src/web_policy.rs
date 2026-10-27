//! Decisions the browser host makes before it touches the page. They need no
//! JavaScript, so they build and are tested natively.

/// Whether `uri` may be handed to `window.open`: web pages and composing
/// links only. `javascript:`, `data:`, `blob:` and `file:` would run script
/// in, or expose content to, the console's origin.
pub fn openable_uri(uri: &str) -> Result<(), String> {
    const OPENABLE: [&str; 4] = ["http", "https", "mailto", "tel"];
    let scheme = uri
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .filter(|scheme| {
            let mut chars = scheme.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
        .ok_or_else(|| format!("not an absolute URI: {uri:?}"))?
        .to_ascii_lowercase();
    if uri.chars().any(char::is_control) {
        return Err("URI contains control characters".to_owned());
    }
    if OPENABLE.contains(&scheme.as_str()) {
        Ok(())
    } else {
        Err(format!("a browser host does not open {scheme}: URIs"))
    }
}

/// The `accept` attribute of an `<input type=file>` for a chooser offering
/// `globs` and `mimetypes`. Empty (anything) when no filter was given or one
/// cannot be expressed: a narrower chooser than the reader would hide files
/// the app could open.
pub fn accept_attribute<'a>(
    globs: impl IntoIterator<Item = &'a str>,
    mimetypes: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut accepted = Vec::new();
    for glob in globs {
        let Some(extension) = glob.strip_prefix("*.") else {
            return String::new();
        };
        if extension.is_empty() || extension.contains(['*', '?', '[', ',', '/']) {
            return String::new();
        }
        accepted.push(format!(".{extension}"));
    }
    for mime in mimetypes {
        if mime.contains(',') || !mime.contains('/') {
            return String::new();
        }
        accepted.push(mime.to_owned());
    }
    accepted.dedup();
    accepted.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_composing_schemes_open() {
        for uri in [
            "https://tilo.so/docs",
            "HTTP://example.com",
            "mailto:ada@example.com",
            "tel:+15551234",
        ] {
            assert_eq!(openable_uri(uri), Ok(()), "{uri}");
        }
        for uri in [
            "javascript:alert(1)",
            " JavaScript:alert(1)",
            "data:text/html,<script>",
            "blob:https://console/1",
            "file:///etc/passwd",
            "settings:display",
            "relative/path",
            "https://example.com/\u{0}x",
        ] {
            assert!(openable_uri(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn accept_lists_extensions_and_types_or_admits_everything() {
        assert_eq!(
            accept_attribute(["*.png", "*.tar.gz"], ["image/png"]),
            ".png,.tar.gz,image/png"
        );
        assert_eq!(accept_attribute([], []), "");
        assert_eq!(accept_attribute(["*.md", "*"], []), "", "All files");
        assert_eq!(accept_attribute(["Makefile"], []), "");
        assert_eq!(accept_attribute(["*.[ch]"], []), "");
        assert_eq!(accept_attribute([], ["text/*"]), "text/*");
    }
}
