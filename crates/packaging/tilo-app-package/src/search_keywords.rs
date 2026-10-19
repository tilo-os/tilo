//! The words an app is also searched by, authored per language next to the
//! app and joined to its manifest when it is packed.
//!
//! `apps/<app>/search/<tag>.ftl` holds one message,
//!
//! ```text
//! search-keywords = youtube, video, videos
//! ```
//!
//! per language tag (`en.ftl`, `de.ftl`). Those files are the only source:
//! `tilo-app.toml` carries no copy, and the packer fills `[package.keywords]`
//! from them before it signs, so an installed manifest has them and nothing
//! committed can disagree with the files. A keyword is a name the app
//! genuinely stands in for or a task it genuinely does; a word that merely
//! sits near the app's category does not belong.

use std::collections::BTreeMap;
use std::path::Path;

/// The message every file defines.
pub const MESSAGE: &str = "search-keywords";

/// The message holding the app's tag line in the file's language: one plain
/// phrase saying what the app does, shown under its name in the Store and in
/// search. Optional per file; the packer joins every language's into
/// `[package.taglines]`.
pub const TAGLINE_MESSAGE: &str = "app-tagline";

/// The words of one file's `search-keywords` message, in order, trimmed and
/// without blanks or repeats; `None` when the file has no such message.
#[must_use]
pub fn parse(source: &str) -> Option<Vec<String>> {
    let value = source.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == MESSAGE && !line.starts_with(char::is_whitespace)).then_some(value)
    })?;
    let mut seen = std::collections::BTreeSet::new();
    Some(
        value
            .split(',')
            .map(str::trim)
            .filter(|word| !word.is_empty() && seen.insert(word.to_lowercase()))
            .map(str::to_owned)
            .collect(),
    )
}

/// The `app-tagline` message of one file, trimmed; `None` when the file has
/// no such message or it is blank.
#[must_use]
pub fn parse_tagline(source: &str) -> Option<String> {
    source
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == TAGLINE_MESSAGE && !line.starts_with(char::is_whitespace))
                .then(|| value.trim().to_owned())
        })
        .filter(|tagline| !tagline.is_empty())
}

/// Every language's tag line for the app in `app_dir`, by tag; empty for an
/// app with no `search/` directory or no `app-tagline` messages.
///
/// # Errors
///
/// Returns why a file cannot be read or is named for no language.
pub fn read_taglines(app_dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut found = BTreeMap::new();
    for (tag, source) in sources(app_dir)? {
        if let Some(tagline) = parse_tagline(&source) {
            found.insert(tag, tagline);
        }
    }
    Ok(found)
}

/// Each `search/<tag>.ftl` of the app in `app_dir` with its language tag.
fn sources(app_dir: &Path) -> Result<Vec<(String, String)>, String> {
    let dir = app_dir.join("search");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ftl") {
            continue;
        }
        let tag = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|tag| {
                (2..=3).contains(&tag.split('-').next().map_or(0, str::len))
                    && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .ok_or_else(|| format!("{}: the file name must be a language tag", path.display()))?
            .to_owned();
        let source =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        found.push((tag, source));
    }
    Ok(found)
}

/// Every language's keywords for the app in `app_dir`, by tag; empty for an
/// app with no `search/` directory.
///
/// # Errors
///
/// Returns why a file cannot be read, is named for no language, or lacks the
/// message.
pub fn read(app_dir: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut found = BTreeMap::new();
    for (tag, source) in sources(app_dir)? {
        let words = parse(&source)
            .filter(|words| !words.is_empty())
            .ok_or_else(|| {
                format!(
                    "{}: no `{MESSAGE}` words",
                    app_dir.join("search").join(format!("{tag}.ftl")).display()
                )
            })?;
        found.insert(tag, words);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tagline_is_its_own_message_and_optional() {
        let source = "search-keywords = notes\napp-tagline =  Notes you write alone or together \n";
        assert_eq!(
            parse_tagline(source).as_deref(),
            Some("Notes you write alone or together")
        );
        assert_eq!(parse_tagline("search-keywords = notes\n"), None);
        assert_eq!(parse_tagline("app-tagline =   \n"), None);
    }

    /// Every first-party app says what it does in English and German, in
    /// one line short enough for a row under its name.
    #[test]
    fn every_app_has_an_english_and_german_tagline() {
        let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        let mut checked = 0;
        for entry in std::fs::read_dir(&apps).expect("apps/") {
            let dir = entry.expect("entry").path();
            if !dir.join("tilo-app.toml").is_file() {
                continue;
            }
            let taglines = read_taglines(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            for language in ["en", "de"] {
                let tagline = taglines
                    .get(language)
                    .unwrap_or_else(|| panic!("{} lacks an {language} app-tagline", dir.display()));
                assert!(
                    tagline.chars().count() <= crate::validate::MAX_TAGLINE_CHARS,
                    "{}: {tagline:?} is longer than a row",
                    dir.display()
                );
                assert!(
                    !tagline.contains(" · ") && !tagline.ends_with('.'),
                    "{}: {tagline:?} is one phrase, without separators or a full stop",
                    dir.display()
                );
            }
            checked += 1;
        }
        assert!(checked > 40, "found only {checked} apps");
    }

    #[test]
    fn the_message_is_read_trimmed_and_without_repeats() {
        let source = "# comment\nsearch-keywords = youtube,  Video , video, ,videos\n";
        assert_eq!(parse(source).unwrap(), ["youtube", "Video", "videos"]);
        assert_eq!(parse("other = x"), None);
    }
}
