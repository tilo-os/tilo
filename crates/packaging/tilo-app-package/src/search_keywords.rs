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

/// Every language's keywords for the app in `app_dir`, by tag; empty for an
/// app with no `search/` directory.
///
/// # Errors
///
/// Returns why a file cannot be read, is named for no language, or lacks the
/// message.
pub fn read(app_dir: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let dir = app_dir.join("search");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(BTreeMap::new());
    };
    let mut found = BTreeMap::new();
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
        let words = parse(&source)
            .filter(|words| !words.is_empty())
            .ok_or_else(|| format!("{}: no `{MESSAGE}` words", path.display()))?;
        found.insert(tag, words);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_is_read_trimmed_and_without_repeats() {
        let source = "# comment\nsearch-keywords = youtube,  Video , video, ,videos\n";
        assert_eq!(parse(source).unwrap(), ["youtube", "Video", "videos"]);
        assert_eq!(parse("other = x"), None);
    }
}
