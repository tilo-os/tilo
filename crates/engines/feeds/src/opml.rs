//! OPML, the format subscription lists travel in: a tiny XML dialect of `<outline>` elements, a
//! feed naming its address in `xmlUrl`, folded into a folder by the outline around it.
//!
//! Reading is tolerant of what people's own exports look like: attribute order, single or double
//! quotes, missing optional attributes, and folders nested in folders. Writing groups the
//! subscriptions under their folders, as the format says.

use serde::Serialize;

use crate::OutLine;

/// One subscription an OPML file lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Outline {
    pub title: String,
    /// The feed's address.
    pub url: String,
    /// Its homepage.
    pub site: String,
    /// The folder the file files it under, by name.
    pub folder: String,
}

/// The subscriptions `text` lists, each with the folder it stands in.
pub fn parse(text: &str) -> Vec<Outline> {
    let mut feeds = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i) != Some(&b'<') {
            i += 1;
            continue;
        }
        let Some(rest) = text.get(i..) else {
            break;
        };
        if rest.starts_with("</outline") {
            folders.pop();
            i += "</outline".len();
            continue;
        }
        if !rest.starts_with("<outline") {
            i += 1;
            continue;
        }
        let Some(rel_end) = rest.find('>') else {
            break;
        };
        let Some(tag) = text.get(i..=i + rel_end) else {
            break;
        };
        let self_closing = tag.trim_end().ends_with("/>");
        let url = attr(tag, "xmlUrl");
        let title = attr(tag, "title")
            .or_else(|| attr(tag, "text"))
            .unwrap_or_default();
        let site = attr(tag, "htmlUrl").unwrap_or_default();
        if let Some(url) = url {
            feeds.push(Outline {
                title: if title.is_empty() {
                    url.clone()
                } else {
                    title
                },
                url,
                site,
                folder: folders.last().cloned().filter(|name| !name.is_empty()).unwrap_or_default(),
            });
            // A feed leaf written with a closing tag is still a leaf: its matching close pops the
            // empty frame this pushes, so the folders around it balance.
            if !self_closing {
                folders.push(String::new());
            }
        } else if !self_closing {
            folders.push(if title.is_empty() {
                "Folder".to_owned()
            } else {
                title
            });
        }
        i += rel_end + 1;
    }
    feeds
}

/// What `tag` names in the attribute `key`, its quotes and escapes read.
fn attr(tag: &str, key: &str) -> Option<String> {
    let mut search = 0;
    while let Some(tail) = tag.get(search..) {
        let Some(rel) = tail.find(key) else {
            break;
        };
        let pos = search + rel;
        // A standalone attribute, led by whitespace or the tag's own name.
        let led = pos == 0
            || tag
                .as_bytes()
                .get(pos - 1)
                .is_some_and(u8::is_ascii_whitespace);
        let after = tag.get(pos + key.len()..)?;
        let after_trim = after.trim_start();
        if led && after_trim.starts_with('=') {
            let rest = after_trim.get(1..)?.trim_start();
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                let body = rest.get(1..)?;
                if let Some(end) = body.find(quote) {
                    let matched = body.get(..end)?;
                    return Some(
                        quick_xml::escape::unescape(matched)
                            .map_or_else(|_| matched.to_owned(), std::borrow::Cow::into_owned),
                    );
                }
            }
        }
        search = pos + key.len();
    }
    None
}

/// The subscriptions as an OPML 2.0 file, grouped under their folders.
pub(crate) fn export(feeds: &[OutLine]) -> String {
    let mut folders: Vec<&str> = feeds
        .iter()
        .map(|feed| feed.folder.as_str())
        .filter(|folder| !folder.is_empty())
        .collect();
    folders.sort_unstable();
    folders.dedup();
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head>\n    \
         <title>Feeds subscriptions</title>\n  </head>\n  <body>\n",
    );
    for folder in &folders {
        let name = quick_xml::escape::escape(*folder);
        out.push_str(&format!(
            "    <outline text=\"{name}\" title=\"{name}\">\n"
        ));
        for feed in feeds.iter().filter(|feed| feed.folder == *folder) {
            out.push_str(&outline(feed, 6));
        }
        out.push_str("    </outline>\n");
    }
    for feed in feeds.iter().filter(|feed| feed.folder.is_empty()) {
        out.push_str(&outline(feed, 4));
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

fn outline(feed: &OutLine, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let format = if feed.format == "atom" { "atom" } else { "rss" };
    format!(
        "{pad}<outline type=\"{format}\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\" htmlUrl=\"{2}\"/>\n",
        quick_xml::escape::escape(&feed.title),
        quick_xml::escape::escape(&feed.url),
        quick_xml::escape::escape(&feed.site),
    )
}

#[cfg(test)]
mod tests;
