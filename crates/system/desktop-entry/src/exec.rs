//! Turning an `Exec=` value into an argv.
//!
//! Two steps the spec keeps separate, and so does this module:
//!
//! 1. **Split.** `Exec` is a command line with its own quoting rules — double quotes group,
//!    and inside them `\` escapes `"`, `` ` ``, `$` and `\`. It is *not* shell syntax: no
//!    globbing, no variable expansion, no pipes — treating it as shell syntax would hand a
//!    `.desktop` file the ability to run anything.
//! 2. **Expand.** `%f %F %u %U %i %c %k` are substituted with what the caller is actually
//!    opening. The deprecated codes (`%d %D %n %N %v %m`) are dropped, per the spec's "must
//!    be removed".
//!
//! Flatpak's `--file-forwarding` markers (`@@`, `@@u`) are understood too: they bracket the
//! argument positions `flatpak run` rewrites into sandbox paths. With no documents to open
//! they bracket nothing, and an empty pair is removed rather than passed on.

use std::path::Path;

/// Why an `Exec=` could not become an argv.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("Exec value has an unterminated quote")]
    UnterminatedQuote,
    #[error("Exec value is empty, so there is no program to run")]
    Empty,
}

/// Split an `Exec=` value into tokens, honouring the spec's quoting. Field codes are left
/// as-is — [`expand`] substitutes them, since only the caller knows what is being opened.
///
/// # Errors
///
/// Returns [`ExecError::UnterminatedQuote`] if a `"` is never closed, or
/// [`ExecError::Empty`] if `raw` contains no tokens at all.
pub fn split_exec(raw: &str) -> Result<Vec<String>, ExecError> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            '"' => {
                has_token = true;
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnterminatedQuote),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            // The four characters the spec says must be escaped inside a quoted argument.
                            Some(esc @ ('"' | '`' | '$' | '\\')) => cur.push(esc),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => return Err(ExecError::UnterminatedQuote),
                        },
                        Some(other) => cur.push(other),
                    }
                }
            }
            other => {
                has_token = true;
                cur.push(other);
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    if out.is_empty() {
        return Err(ExecError::Empty);
    }
    Ok(out)
}

/// What a field code is substituted with.
#[derive(Debug, Clone, Default)]
pub struct ExpandContext<'a> {
    /// Absolute paths the app is being asked to open. Feeds `%f`/`%F`, and `%u`/`%U` as
    /// `file://` URLs when no explicit URL was given.
    pub files: &'a [String],
    /// URLs the app is being asked to open. Feeds `%u`/`%U`; falls back to `files` when empty.
    pub urls: &'a [String],
    pub icon: Option<&'a str>,
    /// The entry's locale-resolved `Name=`, for `%c`.
    pub name: Option<&'a str>,
    pub path: Option<&'a Path>,
}

/// Expand field codes in an already-split argv. Codes expand *within* a token, so
/// `--file=%f` works; the list codes (`%F`, `%U`) and `%i` expand to zero or more tokens,
/// only meaningful when the code is the whole token — a list code embedded in a larger token
/// is dropped rather than concatenated into nonsense.
pub fn expand(argv: &[String], ctx: &ExpandContext<'_>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let urls: Vec<String> = if ctx.urls.is_empty() {
        ctx.files.iter().map(|f| path_to_url(f)).collect()
    } else {
        ctx.urls.to_vec()
    };

    for token in argv {
        match token.as_str() {
            "%F" => out.extend(ctx.files.iter().cloned()),
            "%U" => out.extend(urls.iter().cloned()),
            "%i" => {
                if let Some(icon) = ctx.icon.filter(|s| !s.is_empty()) {
                    out.push("--icon".to_string());
                    out.push(icon.to_string());
                }
            }
            _ => {
                if let Some(expanded) = expand_in_token(token, ctx, &urls) {
                    out.push(expanded);
                }
            }
        }
    }
    strip_empty_forwarding_markers(&out)
}

/// Substitute the single-value codes inside one token. `None` means the token expanded to
/// nothing and must be dropped entirely — what a bare `%f` with no file to open has to do,
/// since an empty string would look like a filename of `""`.
fn expand_in_token(token: &str, ctx: &ExpandContext<'_>, urls: &[String]) -> Option<String> {
    if !token.contains('%') {
        return Some(token.to_string());
    }
    let mut out = String::with_capacity(token.len());
    let mut dropped_value = false;
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('f') => match ctx.files.first() {
                Some(f) => out.push_str(f),
                None => dropped_value = true,
            },
            Some('u') => match urls.first() {
                Some(u) => out.push_str(u),
                None => dropped_value = true,
            },
            Some('c') => {
                if let Some(name) = ctx.name {
                    out.push_str(name);
                }
            }
            Some('k') => {
                if let Some(path) = ctx.path {
                    out.push_str(&path.to_string_lossy());
                }
            }
            // The list codes are only valid as a whole token; reaching here means one was embedded in a
            // larger argument, which the spec forbids. Drop the whole token rather than guess.
            Some('F' | 'U' | 'i') => return None,
            // Everything else — the deprecated codes (`%d %D %n %N %v %m`, spec: "must be
            // removed"), a trailing `%`, or any other unknown letter — keeps neither the `%`
            // nor the letter: an unknown code is not a literal.
            Some(_) | None => {}
        }
    }
    // Two ways a token disappears: a value code with no value (`--file=%f` must not become
    // `--file=`, an empty filename) or a token that was nothing but codes that expanded away
    // (a deprecated code). Both leave no argument behind rather than an empty one.
    if dropped_value || out.is_empty() {
        return None;
    }
    Some(out)
}

/// Remove `@@ … @@` / `@@u … @@` pairs that ended up empty. `flatpak run --file-forwarding`
/// uses these to mark arguments it should rewrite for the sandbox; when the launch opens
/// nothing, the pair ends up bracketing empty space — `flatpak` tolerates that, but it's
/// noise in every log and `ps` listing, so it goes.
fn strip_empty_forwarding_markers(argv: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let mut iter = argv.iter().peekable();
    // `for tok in &mut iter` would hold the borrow for the whole loop, so the
    // `peek` below could not look ahead; `while let` releases it each step.
    while let Some(tok) = iter.next() {
        let is_open = tok == "@@" || tok == "@@u";
        if is_open && iter.peek().is_some_and(|next| next.as_str() == "@@") {
            iter.next(); // consume the matching close marker too
            continue;
        }
        out.push(tok.clone());
    }
    out
}

/// `/home/u/a b.txt` → `file:///home/u/a%20b.txt`, so `%u` gets a well-formed URL. A string
/// that is already a URL, or a relative path, passes through unchanged.
pub fn path_to_url(path: &str) -> String {
    if path.contains("://") {
        return path.to_string();
    }
    url::Url::from_file_path(path).map_or_else(|()| path.to_string(), String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str) -> Vec<String> {
        split_exec(s).expect("splits")
    }

    #[test]
    fn exec_lines_split_on_quotes_without_a_shell() {
        {
            assert_eq!(
                split(r#"/usr/bin/foo "a b" c"#),
                vec!["/usr/bin/foo", "a b", "c"]
            );
            assert_eq!(split(r#"foo "a\"b""#), vec!["foo", r#"a"b"#]);
            assert_eq!(split(r#"foo "a\\b""#), vec!["foo", r"a\b"]);
            // Shell metacharacters are ordinary text, not operators.
            assert_eq!(
                split("foo a;rm -rf /|b"),
                vec!["foo", "a;rm", "-rf", "/|b"],
                "Exec is not shell syntax and must never be treated as such"
            );
        }
        {
            assert_eq!(split_exec(r#"foo "bar"#), Err(ExecError::UnterminatedQuote));
            assert_eq!(split_exec("   "), Err(ExecError::Empty));
        }
        {
            let ctx = ExpandContext::default();
            assert_eq!(expand(&split("app 100%%"), &ctx), vec!["app", "100%"]);
        }
        {
            let ctx = ExpandContext::default();
            assert_eq!(expand(&split("app %d %D %n %N %v %m"), &ctx), vec!["app"]);
        }
    }

    #[test]
    fn field_codes_expand_documents_icons_names_and_paths() {
        {
            let files = vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()];
            let ctx = ExpandContext {
                files: &files,
                ..Default::default()
            };
            assert_eq!(
                expand(&split("app %F"), &ctx),
                vec!["app", "/tmp/a.txt", "/tmp/b.txt"]
            );
            assert_eq!(expand(&split("app %f"), &ctx), vec!["app", "/tmp/a.txt"]);
            assert_eq!(
                expand(&split("app %U"), &ctx),
                vec!["app", "file:///tmp/a.txt", "file:///tmp/b.txt"],
                "%U falls back to file:// URLs for the files"
            );
        }
        // Phase `field_codes_with_nothing_to_open_leave_no_argument_behind`.
        // A launch with nothing to open must not pass an empty argument — an app reading argv[1]
        // would see a filename of `""`.
        {
            let ctx = ExpandContext::default();
            assert_eq!(expand(&split("app %f"), &ctx), vec!["app"]);
            assert_eq!(expand(&split("app %F"), &ctx), vec!["app"]);
            assert_eq!(expand(&split("app %U %u"), &ctx), vec!["app"]);
            assert_eq!(
                expand(&split("app --file=%f"), &ctx),
                vec!["app"],
                "`--file=` with no filename after it is worse than no argument"
            );
            // A genuinely empty quoted argument is not a field code and survives.
            assert_eq!(expand(&split(r#"app "" x"#), &ctx), vec!["app", "", "x"]);
        }
        {
            let files = vec!["/tmp/a.txt".to_string()];
            let ctx = ExpandContext {
                files: &files,
                ..Default::default()
            };
            assert_eq!(
                expand(&split("app --file=%f"), &ctx),
                vec!["app", "--file=/tmp/a.txt"]
            );
        }
        {
            let path = Path::new("/usr/share/applications/x.desktop");
            let ctx = ExpandContext {
                icon: Some("text-editor"),
                name: Some("Text Editor"),
                path: Some(path),
                ..Default::default()
            };
            assert_eq!(
                expand(&split("app %i %c %k"), &ctx),
                vec![
                    "app",
                    "--icon",
                    "text-editor",
                    "Text Editor",
                    "/usr/share/applications/x.desktop"
                ]
            );
            // No icon declared: %i contributes nothing, not an empty --icon.
            let no_icon = ExpandContext::default();
            assert_eq!(expand(&split("app %i"), &no_icon), vec!["app"]);
        }
        {
            assert_eq!(path_to_url("/tmp/a b.txt"), "file:///tmp/a%20b.txt");
            assert_eq!(path_to_url("/tmp/a#b"), "file:///tmp/a%23b");
            assert_eq!(
                path_to_url("https://example.com/x"),
                "https://example.com/x",
                "something already a URL is left alone"
            );
        }
    }

    #[test]
    fn flatpak_exports_expand_to_runnable_argv_with_document_markers() {
        {
            let raw = "/usr/bin/flatpak run --branch=stable --arch=x86_64 \
                       --command=spotify --file-forwarding com.spotify.Client @@u %U @@";
            let argv = expand(&split(raw), &ExpandContext::default());
            assert_eq!(
                argv,
                vec![
                    "/usr/bin/flatpak",
                    "run",
                    "--branch=stable",
                    "--arch=x86_64",
                    "--command=spotify",
                    "--file-forwarding",
                    "com.spotify.Client",
                ],
                "with nothing to open, the file-forwarding markers bracket \
                 nothing and are dropped"
            );
        }
        {
            let raw = "/usr/bin/flatpak run --file-forwarding com.spotify.Client @@u %U @@";
            let files = vec!["/tmp/a.txt".to_string()];
            let argv = expand(
                &split(raw),
                &ExpandContext {
                    files: &files,
                    ..Default::default()
                },
            );
            assert_eq!(
                argv,
                vec![
                    "/usr/bin/flatpak",
                    "run",
                    "--file-forwarding",
                    "com.spotify.Client",
                    "@@u",
                    "file:///tmp/a.txt",
                    "@@",
                ]
            );
        }
    }
}
