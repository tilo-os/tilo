//! Rows the desktop describes and the panel shows: the Devices section's
//! device fabric rows and the Clipboard section's copies.
//!
//! The desktop holds the device fabric and the clipboard history, so it
//! describes each row ([`HostedRow`]) while its section shows; the panel
//! lists the rows and names which of a row's buttons was pressed in its
//! effects ([`crate::RowPart`]).

use ui::icons::Icon;

/// What a row offers besides a press on its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedActions {
    None,
    /// The row's one action, named on a button (stop, cancel, try again).
    Select(String),
    /// Two choices: allow and decline.
    Pair {
        first: (Icon, String),
        second: (Icon, String),
    },
    /// Driving another device with this machine's pointer. Where its
    /// display stands is arranged in its settings pane, which a press on
    /// the row opens.
    Control {
        name: String,
        /// This machine's pointer is driving it now.
        controlling: bool,
        /// It can be driven: online and not this machine.
        controllable: bool,
    },
    /// A copy's pin and remove.
    Clipboard {
        pinned: bool,
        pin: String,
        remove: String,
    },
}

/// One row's face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedRow {
    /// Names the row in the panel's effects: what the desktop knows it by.
    pub key: String,
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    /// The row is the chosen one (the media source the volume follows).
    pub selected: bool,
    pub enabled: bool,
    pub actions: HostedActions,
}

/// One copy the desktop keeps, as the Clipboard section lists it: the row's
/// face without its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardCopy {
    /// Names the copy in the panel's effects.
    pub key: String,
    /// The format its bytes are in.
    pub mime: String,
    /// Its first line, when it is text.
    pub text: Option<String>,
    /// When it was copied, in Unix milliseconds.
    pub created_ms: u64,
    /// Whether the person keeps it when the history is cleared.
    pub pinned: bool,
    /// The device another of the person's devices shared it from.
    pub from: Option<String>,
}

/// Formats whose copies are pictures.
const IMAGE_MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// The longest title a text copy shows before the line is cut.
const TITLE_CHARS: usize = 120;

/// The Clipboard section and its row for `copies`, newest first, as of
/// `now_ms`.
#[must_use]
pub fn clipboard_view(copies: &[ClipboardCopy], now_ms: u64) -> crate::ClipboardView {
    crate::ClipboardView {
        total: copies.len(),
        // The section says what it is showing: how many, or what is
        // searched for.
        caption: String::new(),
        rows: copies
            .iter()
            .map(|copy| clipboard_row(copy, now_ms))
            .collect(),
    }
}

/// One copy's row: what it holds, how long ago, and where it came from.
fn clipboard_row(copy: &ClipboardCopy, now_ms: u64) -> HostedRow {
    // The format says what the row is; the text is what it holds, which for
    // a copy of files is the first file's name.
    let files = ui::dnd::is_file_mime(&copy.mime);
    let title = if let Some(text) = copy.text.as_deref() {
        title_of(text)
    } else if files {
        crate::i18n::t("qs-copy-files")
    } else if IMAGE_MIMES.contains(&copy.mime.as_str()) {
        crate::i18n::t("qs-copy-image")
    } else {
        crate::i18n::t("qs-copy-formatted")
    };
    let icon = if files {
        Icon::Folder
    } else if IMAGE_MIMES.contains(&copy.mime.as_str()) {
        Icon::Image
    } else {
        Icon::FileText
    };
    let seconds = now_ms.saturating_sub(copy.created_ms) / 1000;
    let age = ui::format::relative(i64::try_from(seconds).unwrap_or(i64::MAX));
    let detail = match &copy.from {
        Some(device) => crate::i18n::t_args(
            "qs-copy-detail-from",
            &[("age", age.into()), ("device", device.as_str().into())],
        ),
        None => crate::i18n::t_args("qs-copy-detail-this-device", &[("age", age.into())]),
    };
    HostedRow {
        key: copy.key.clone(),
        title,
        detail,
        icon,
        selected: false,
        enabled: true,
        actions: HostedActions::Clipboard {
            pinned: copy.pinned,
            pin: String::new(),
            remove: String::new(),
        },
    }
}

/// A text copy's title: its first line with its runs of whitespace closed
/// up, cut at [`TITLE_CHARS`].
fn title_of(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let closed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if closed.chars().count() <= TITLE_CHARS {
        return closed;
    }
    let cut: String = closed.chars().take(TITLE_CHARS).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The format a text copy travels in.
    const TEXT: &str = "text/plain;charset=utf-8";

    fn copy(mime: &str, text: Option<&str>) -> ClipboardCopy {
        ClipboardCopy {
            key: "k".to_owned(),
            mime: mime.to_owned(),
            text: text.map(str::to_owned),
            created_ms: 0,
            pinned: false,
            from: None,
        }
    }

    #[test]
    fn a_copy_says_when_and_where_it_was_copied() {
        // Case `a_copy_says_which_device_it_came_from`:
        // A copy shared from another device names it, and one made here says
        // it came from this computer, both after how long ago it was made.
        let now = 1_000_000;
        let mut here = copy(TEXT, Some("made here"));
        here.created_ms = now - 30_000;
        let mut there = copy(TEXT, Some("made there"));
        there.created_ms = now - 30_000;
        there.from = Some("laptop".to_owned());
        let view = clipboard_view(&[here, there], now);
        assert!(
            view.rows[0].detail.ends_with(", from this computer"),
            "a copy made here: {}",
            view.rows[0].detail
        );
        assert!(
            view.rows[1].detail.ends_with(", from laptop"),
            "a copy shared from a laptop: {}",
            view.rows[1].detail
        );
        assert!(
            !view.rows[1].detail.starts_with(", from"),
            "the age comes first: {}",
            view.rows[1].detail
        );
    }

    #[test]
    fn a_copied_file_is_a_folder_row_named_after_the_first_file() {
        // Case `a_file_copy_lists_as_the_files_it_is`:
        // A copy of files is not formatted text: it is a folder, named by
        // the first file, so the row says what was copied.
        let view = clipboard_view(
            &[copy("text/uri-list", Some("holiday.png"))],
            ui::time::unix_now_ms(),
        );
        assert_eq!(view.rows[0].title, "holiday.png");
        assert_eq!(view.rows[0].icon, Icon::Folder);

        // Case `a_file_copy_without_a_name_still_says_files`:
        // With no path to show, the row still says files rather than
        // claiming a format it does not hold.
        let view = clipboard_view(&[copy("x-special/gnome-copied-files", None)], 0);
        assert_eq!(view.rows[0].icon, Icon::Folder);
        assert_ne!(view.rows[0].title, "");
    }
}
