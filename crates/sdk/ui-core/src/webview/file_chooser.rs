//! A page's file chooser, opened by a person's own click on a file input.
//!
//! An app never draws a file picker of its own: the desktop's picker is the
//! one place a person chooses a file, and where their files are granted. So
//! an engine that can holds the page's chooser and says so
//! ([`WebViewEvent::FileChooser`](super::WebViewEvent::FileChooser)); the
//! embedder asks the host's picker and answers with exactly the paths the
//! person picked, or with none
//! ([`WebViewCommand::AnswerFileChooser`](super::WebViewCommand::AnswerFileChooser)).
//! The page then receives what a platform panel's answer gives it: its own
//! trusted `input` and `change` events, or `cancel`.
//!
//! Only a person's click arrives here. A chooser a page action opened is the
//! engine's own to hold for that action, and nothing an action does answers
//! a person's. The engine ends a chooser nobody answers
//! ([`WebViewEvent::FileChooserClosed`](super::WebViewEvent::FileChooserClosed)),
//! so no page waits on a picker for good.

/// The longest title a chooser carries, in bytes.
pub const MAX_FILE_CHOOSER_TITLE: usize = 256;
/// The most types a chooser says its input accepts.
pub const MAX_FILE_CHOOSER_TYPES: usize = 256;
/// The longest one accepted type, in bytes.
pub const MAX_FILE_CHOOSER_TYPE: usize = 128;
/// The most paths one answer names.
pub const MAX_FILE_CHOOSER_PATHS: usize = 4096;
/// The longest one path of an answer, in bytes.
pub const MAX_FILE_CHOOSER_PATH: usize = 4096;

/// What the page's input takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChooserMode {
    /// One file.
    One,
    /// One file or more (`multiple`).
    Several,
    /// One folder, with what it holds (`webkitdirectory`).
    Folder,
}

/// A file chooser the page waits on until it is answered or the engine says
/// it no longer waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChooser {
    pub id: u32,
    pub mode: FileChooserMode,
    /// The chooser's title, when the engine names one; empty for the
    /// embedder's own.
    pub title: String,
    /// What the input accepts, as a hint to the picker and never a rule the
    /// page may rely on: MIME types (`image/*`, `application/pdf`) and file
    /// extensions (`.pdf`).
    pub accept: Vec<String>,
}

/// Whether `accepted` is one type a chooser may name: a file extension
/// (`.pdf`) or a MIME type (`image/*`), in visible ASCII.
pub fn accepted_type(accepted: &str) -> bool {
    let extension = accepted.len() > 1 && accepted.starts_with('.') && !accepted.contains('/');
    let mime = !accepted.starts_with('.') && accepted.contains('/');
    (extension || mime)
        && accepted.len() <= MAX_FILE_CHOOSER_TYPE
        && accepted.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether `paths` is an answer an engine takes: within its bounds, and
/// every path absolute, so none is resolved against the engine's own
/// working folder. No paths at all cancels the chooser.
pub fn answers(paths: &[String]) -> bool {
    paths.len() <= MAX_FILE_CHOOSER_PATHS
        && paths
            .iter()
            .all(|path| path.starts_with('/') && path.len() <= MAX_FILE_CHOOSER_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_names_absolute_paths_within_the_engines_bounds() {
        assert!(answers(&[]), "no paths cancels");
        assert!(answers(&["/home/ada/proof.pdf".into()]));
        for refused in ["proof.pdf", "", "./proof.pdf", "~/proof.pdf"] {
            assert!(!answers(&[refused.into()]), "{refused}");
        }
        assert!(!answers(&[format!(
            "/{}",
            "x".repeat(MAX_FILE_CHOOSER_PATH)
        )]));
        assert!(answers(&vec!["/a".to_string(); MAX_FILE_CHOOSER_PATHS]));
        assert!(!answers(&vec![
            "/a".to_string();
            MAX_FILE_CHOOSER_PATHS + 1
        ]));
    }

    #[test]
    fn an_accepted_type_is_an_extension_or_a_mime_type() {
        for taken in [".pdf", ".tar.gz", "image/*", "application/pdf"] {
            assert!(accepted_type(taken), "{taken}");
        }
        for refused in ["", ".", "pdf", "./x", "image/* ", "im age/png", ".p\u{e9}f"] {
            assert!(!accepted_type(refused), "{refused}");
        }
        assert!(!accepted_type(&format!(
            ".{}",
            "x".repeat(MAX_FILE_CHOOSER_TYPE)
        )));
    }
}
