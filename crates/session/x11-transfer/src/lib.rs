//! What crosses between X11 and Wayland programs: the names a format goes by on each side.
//!
//! X11 programs name text by the old selection targets (`UTF8_STRING`, `STRING`, `TEXT`,
//! `COMPOUND_TEXT`), Wayland programs by MIME type (`text/plain;charset=utf-8`, `text/plain`).
//! An X11 server for Wayland passes the names through as they are, so a copy from a program that
//! offers only `UTF8_STRING` could not be pasted into one that asks for `text/plain`. Whoever
//! carries an offer from one side to the other (the compositor for its own X11 displays, the app
//! keeper for a kept launch's) offers text under every name ([`with_text_names`]) and serves a
//! request for any of them with the best name the source has ([`pick_format`]).
//!
//! Drags cross too: [`xdnd`] is the XDND protocol as plain data, [`bridge`] an X11 connection that
//! speaks it for whoever carries a drag between an X11 display and Wayland.

pub mod bridge;
pub mod windows;
pub mod xdnd;

/// Cap on the bytes one transfer carries.
pub const MAX_SELECTION_BYTES: usize = 8 * 1024 * 1024;

/// Write `bytes` to a reader's pipe off the caller's loop: a reader that is slow or never reads
/// must not stall it. Closing the pipe is the end.
pub fn serve(fd: std::os::fd::OwnedFd, bytes: Vec<u8>) {
    std::thread::spawn(move || {
        use std::io::Write as _;
        if let Err(error) = std::fs::File::from(fd).write_all(&bytes) {
            tracing::debug!(%error, "selection reader went away mid-transfer");
        }
    });
}

use tilo_mime::TEXT_PREFERENCE;
pub use tilo_mime::{TEXT_MIME, TEXT_MIMES};

/// Cap on the formats one offer advertises.
pub const MAX_MIMES: usize = 16;
/// Cap on one format's name.
const MAX_MIME_BYTES: usize = 128;

/// Whether `mime` is one of text's names.
#[must_use]
pub fn is_text_mime(mime: &str) -> bool {
    tilo_mime::is_text(mime)
}

/// Whether `mime` can name a format: not empty, bounded, no whitespace or control characters.
#[must_use]
pub fn valid_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.len() <= MAX_MIME_BYTES
        && !mime.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// `mimes` as they are, followed by text's names they lack when they offer text at all.
#[must_use]
pub fn with_text_names(mimes: &[String]) -> Vec<String> {
    let mut named = mimes.to_vec();
    if mimes.iter().any(|mime| is_text_mime(mime)) {
        for alias in TEXT_MIMES {
            if !named.iter().any(|mime| mime == alias) {
                named.push(alias.to_owned());
            }
        }
    }
    named
}

/// The names an offer of `offered` gains when `mime` is offered too: `mime` itself and, for its
/// first text name, text's other names.
#[must_use]
pub fn names_added(offered: &[String], mime: &str) -> Vec<String> {
    let before = with_text_names(offered);
    let mut all = offered.to_vec();
    all.push(mime.to_owned());
    with_text_names(&all)
        .into_iter()
        .filter(|name| !before.contains(name))
        .collect()
}

/// The formats an offer advertises: valid names only, bounded, and text under every name text goes
/// by, UTF-8 first so old programs do not pick an ambiguous X11 name.
pub fn normalized_mimes(mimes: impl IntoIterator<Item = String>) -> Vec<String> {
    let valid: Vec<String> = mimes
        .into_iter()
        .filter(|mime| valid_mime(mime))
        .take(MAX_MIMES)
        .collect();
    let mut mimes = with_text_names(&valid);
    mimes.truncate(MAX_MIMES);
    mimes.sort();
    mimes.dedup();
    mimes.sort_by_key(|mime| (mime != TEXT_MIME, mime.clone()));
    mimes
}

/// Which of `available` serves a request for `wanted`: the same name, or for text whichever text
/// name is there, UTF-8 first.
#[must_use]
pub fn pick_format(wanted: &str, available: &[String]) -> Option<String> {
    if available.iter().any(|name| name == wanted) {
        return Some(wanted.to_owned());
    }
    if !is_text_mime(wanted) {
        return None;
    }
    TEXT_PREFERENCE
        .iter()
        .find(|name| available.iter().any(|offered| offered == *name))
        .map(|name| (*name).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|mime| (*mime).to_owned()).collect()
    }

    tilo_test_env::scenario! {
        /// The names text is offered and read under.
        fn text_goes_by_every_name_on_both_sides;

        phase text_offers_go_by_every_text_name_utf8_first_and_bad_names_are_dropped;
        {
            let mimes = normalized_mimes(strings(&["text/plain", "image/png", "bad mime", ""]));
            assert_eq!(mimes.first().map(String::as_str), Some(TEXT_MIME));
            for alias in TEXT_MIMES {
                assert!(
                    mimes.iter().any(|mime| mime == alias),
                    "{alias} missing: {mimes:?}"
                );
            }
            assert!(mimes.iter().any(|mime| mime == "image/png"));
            assert!(
                !mimes
                    .iter()
                    .any(|mime| mime.contains(' ') || mime.is_empty())
            );
            assert_eq!(
                normalized_mimes(strings(&["image/png"])),
                strings(&["image/png"])
            );
        }

        phase an_old_x11_programs_text_is_offered_under_the_wayland_names;
        {
            let named = with_text_names(&strings(&["TARGETS", "UTF8_STRING"]));
            assert_eq!(named.get(..2), Some(&strings(&["TARGETS", "UTF8_STRING"])[..]), "what was offered stays first");
            assert!(named.contains(&TEXT_MIME.to_owned()));
            assert!(named.contains(&"text/plain".to_owned()));
            assert_eq!(with_text_names(&strings(&["image/png"])), strings(&["image/png"]));

            let offered = strings(&["image/png"]);
            assert_eq!(names_added(&offered, "image/jpeg"), strings(&["image/jpeg"]));
            let added = names_added(&offered, "STRING");
            assert_eq!(added.len(), TEXT_MIMES.len(), "the first text name brings the others: {added:?}");
            let offered = strings(&["STRING"]);
            assert!(names_added(&offered, "UTF8_STRING").is_empty(), "text's names were offered already");
        }

        phase requests_are_served_by_the_same_name_or_any_text_name;
        {
            let offered = strings(&["text/uri-list", "UTF8_STRING", "STRING"]);
            assert_eq!(
                pick_format("text/uri-list", &offered).as_deref(),
                Some("text/uri-list")
            );
            assert_eq!(
                pick_format(TEXT_MIME, &offered).as_deref(),
                Some("UTF8_STRING")
            );
            assert_eq!(pick_format("text/plain", &offered).as_deref(), Some("UTF8_STRING"));
            assert_eq!(pick_format("image/png", &offered), None);
            let wayland = strings(&["text/plain", TEXT_MIME]);
            assert_eq!(
                pick_format("UTF8_STRING", &wayland).as_deref(),
                Some(TEXT_MIME)
            );
            assert_eq!(pick_format("STRING", &strings(&["text/plain"])).as_deref(), Some("text/plain"));
        }
    }
}
