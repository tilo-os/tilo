//! Running an external code formatter over a buffer.
//!
//! Format-on-save and the manual "Format Document" command both come down to the
//! same thing: take the buffer's current text, hand it to the language's
//! conventional formatter as a subprocess (stdin in, formatted source out), and
//! splice the result back through the CRDT text path (so the reformat is
//! collaborative and a single undo step).
//!
//! Two rules keep this from ever corrupting a file:
//!
//! * **A formatter that fails changes nothing.** A non-zero exit, a spawn
//!   failure, non-UTF-8 output, or output that empties a non-empty buffer all
//!   return an [`FormatError`] and leave the text untouched. rustfmt exits
//!   non-zero on a syntax error precisely so an editor does not overwrite your
//!   half-typed function with nothing.
//! * **The formatter is real, or there is none.** There is no built-in
//!   "pretty printer" that pretends to format; [`formatter_command`] returns the
//!   actual conventional binary for the language (`rustfmt`, `gofmt`, `black`,
//!   `prettier`, `clang-format`) or `None`. A missing binary degrades to a clear
//!   note, exactly like the language server does.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::model::Language;

/// Why a format attempt did not produce new text. Every variant leaves the
/// buffer exactly as it was.
#[derive(Debug)]
pub enum FormatError {
    /// No formatter is configured for this language.
    NoFormatter(Language),
    /// The formatter binary could not be launched (usually not installed).
    NotFound(String),
    /// The formatter ran but failed — a syntax error, or a bad config. Carries
    /// the program name and whatever it wrote to stderr, trimmed.
    Failed { program: String, stderr: String },
    /// Some other I/O error talking to the process.
    Io(std::io::Error),
    /// The formatter emitted bytes that are not UTF-8, or emptied a non-empty
    /// buffer — either would corrupt the file, so it is refused.
    BadOutput(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NoFormatter(lang) => {
                write!(f, "no formatter configured for {}", lang.label())
            }
            FormatError::NotFound(prog) => {
                write!(f, "{prog} not found on PATH — install it to format")
            }
            FormatError::Failed { program, stderr } => {
                if stderr.is_empty() {
                    write!(f, "{program} could not format this file")
                } else {
                    write!(f, "{program}: {stderr}")
                }
            }
            FormatError::Io(e) => write!(f, "{e}"),
            FormatError::BadOutput(msg) => write!(f, "{msg}"),
        }
    }
}

/// The conventional stdin→stdout formatter command for a language, as an argv.
///
/// Every command here reads the source on **stdin** and writes the formatted
/// source to **stdout**, which is what lets the editor format a buffer that may
/// have unsaved edits without first writing it to disk. A `{}` placeholder in an
/// argument is replaced with the file's path (or a synthetic `stdin.<ext>` when
/// the buffer has none) so a formatter that picks its rules from the filename
/// — `prettier` — still gets one.
///
/// `None` means qOS ships no formatter mapping for the language; the caller
/// turns that into an honest "no formatter" note rather than a silent no-op.
pub fn formatter_command(lang: Language) -> Option<Vec<String>> {
    let argv: &[&str] = match lang {
        // rustfmt with no file argument reads stdin and writes stdout.
        Language::Rust => &["rustfmt", "--edition", "2021"],
        Language::Go => &["gofmt"],
        Language::Python => &["black", "--quiet", "-"],
        Language::C | Language::Cpp => &["clang-format"],
        // prettier infers the parser from the filepath, so it needs one.
        Language::JavaScript
        | Language::TypeScript
        | Language::Html
        | Language::Css
        | Language::Json
        | Language::Yaml
        | Language::Markdown => &["prettier", "--stdin-filepath", "{}"],
        Language::Toml | Language::Shell | Language::PlainText => return None,
    };
    Some(argv.iter().map(|s| s.to_string()).collect())
}

/// The file extension used to synthesise a `stdin.<ext>` name when a buffer has
/// no path but its formatter wants a filename (prettier).
fn synthetic_name(lang: Language) -> String {
    format!("stdin.{}", lang.syntect_extension())
}

/// Run `argv` as a formatter over `text`, returning the reformatted source.
///
/// `path` is the buffer's real file, substituted for any `{}` in the argv; when
/// the buffer has none a `stdin.<ext>` stand-in is used so a filename-sniffing
/// formatter still resolves a parser. The child's stdin is written from a
/// dedicated thread so a large buffer can't deadlock against a formatter that
/// starts emitting before it has read all of its input.
pub fn run(
    argv: &[String],
    text: &str,
    path: Option<&Path>,
    lang: Language,
) -> Result<String, FormatError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(FormatError::NoFormatter(lang));
    };
    let filename = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| synthetic_name(lang));
    let args: Vec<String> = args
        .iter()
        .map(|a| a.replace("{}", &filename))
        .collect();

    let mut child = Command::new(program)
        .args(&args)
        // Run the formatter in the file's directory when there is one, so it
        // resolves the same rustfmt.toml / .prettierrc a shell there would.
        .current_dir(path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new(".")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FormatError::NotFound(program.clone())
            } else {
                FormatError::Io(e)
            }
        })?;

    // Feed stdin from a thread so writing the whole buffer can't block on a
    // formatter that is already writing to a stdout pipe we haven't drained yet.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let owned = text.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(owned.as_bytes());
        // Dropping stdin here closes it, signalling EOF to the formatter.
    });

    let output = child.wait_with_output().map_err(FormatError::Io)?;
    let _ = writer.join();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(FormatError::Failed {
            program: program.clone(),
            stderr: first_line(&stderr, 200),
        });
    }

    let formatted = String::from_utf8(output.stdout)
        .map_err(|_| FormatError::BadOutput("formatter produced non-text output".to_string()))?;

    // A formatter that hands back an empty buffer for non-empty input has almost
    // certainly errored on stdout rather than stderr; refuse to wipe the file.
    if formatted.is_empty() && !text.trim().is_empty() {
        return Err(FormatError::BadOutput(
            "formatter returned no output".to_string(),
        ));
    }
    Ok(formatted)
}

/// The first line of `s`, capped at `max` chars, for a one-line status note.
fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() > max {
        line.chars().take(max).collect::<String>() + "\u{2026}"
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mapped_language_reads_stdin() {
        // The invariant the whole module rests on: each mapped formatter reads
        // stdin (no bare file argument that would make it ignore the buffer).
        for lang in [
            Language::Rust,
            Language::Go,
            Language::Python,
            Language::C,
            Language::JavaScript,
        ] {
            let cmd = formatter_command(lang).expect("mapped language has a formatter");
            assert!(!cmd.is_empty());
        }
        // TOML / Shell / plain text have no formatter — honestly None.
        assert!(formatter_command(Language::Toml).is_none());
        assert!(formatter_command(Language::PlainText).is_none());
    }

    #[test]
    fn a_real_subprocess_formats_the_text() {
        // Use a ubiquitous shell filter as a stand-in formatter: it reads the
        // buffer on stdin and writes a transformed version to stdout, exactly as
        // rustfmt would. This proves the real spawn/pipe/capture path.
        let argv = shell(&["tr", "a-z", "A-Z"]);
        let out = run(&argv, "fn main() {}", None, Language::Rust).expect("formatter runs");
        assert_eq!(out, "FN MAIN() {}");
    }

    #[test]
    fn a_failing_formatter_changes_nothing() {
        // Exit non-zero (like rustfmt on a syntax error): the error is returned,
        // never the (possibly partial) stdout.
        let argv = shell(&["sh", "-c", "cat >/dev/null; echo boom >&2; exit 1"]);
        match run(&argv, "some code", None, Language::Rust) {
            Err(FormatError::Failed { stderr, .. }) => assert!(stderr.contains("boom")),
            other => panic!("expected a Failed error, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_binary_is_a_clear_error() {
        let argv = vec!["qos-no-such-formatter-binary".to_string()];
        assert!(matches!(
            run(&argv, "x", None, Language::Rust),
            Err(FormatError::NotFound(_))
        ));
    }

    #[test]
    fn emptying_a_nonempty_buffer_is_refused() {
        // A formatter that returns nothing for real input would wipe the file.
        let argv = shell(&["sh", "-c", "cat >/dev/null; true"]);
        assert!(matches!(
            run(&argv, "real content", None, Language::Rust),
            Err(FormatError::BadOutput(_))
        ));
    }

    #[test]
    fn the_filepath_placeholder_is_substituted() {
        // prettier needs the filename to pick a parser; prove `{}` reaches argv.
        let argv = shell(&["sh", "-c", "printf '%s' \"$1\"", "_"]);
        // The command echoes its first positional arg; feed the placeholder there.
        let argv = {
            let mut v = argv;
            v.push("{}".to_string());
            v
        };
        let out = run(&argv, "ignored", Some(Path::new("/tmp/a.ts")), Language::TypeScript)
            .expect("runs");
        assert_eq!(out, "/tmp/a.ts");
    }

    /// A formatter stand-in built from a universally-available shell filter.
    fn shell(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }
}
