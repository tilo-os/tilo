//! OPML, the format subscription lists travel in: a tiny XML dialect of `<outline>` elements, a
//! feed naming its address in `xmlUrl`, folded into a folder by the outline around it.
//!
//! Reading is tolerant of what people's own exports look like: attribute order, single or double
//! quotes, missing optional attributes, and folders nested in folders. Writing groups the
//! subscriptions under their folders, as the format says.

use quick_xml::events::{BytesStart, Event};
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

/// The subscriptions `text` lists, each with the folder it stands in. What does not read as XML
/// (a close with no open, text that only looks like tags) is passed over, and reading goes on
/// after it.
pub fn parse(text: &str) -> Vec<Outline> {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    let mut feeds = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    loop {
        let before = reader.buffer_position();
        match reader.read_event() {
            Ok(Event::Start(tag)) if tag.local_name().as_ref() == b"outline" => {
                opened(&tag, false, &mut folders, &mut feeds);
            }
            Ok(Event::Empty(tag)) if tag.local_name().as_ref() == b"outline" => {
                opened(&tag, true, &mut folders, &mut feeds);
            }
            Ok(Event::End(tag)) if tag.local_name().as_ref() == b"outline" => {
                folders.pop();
            }
            Ok(Event::Eof) => break,
            // The reader stands past what it could not read; one that cannot move on ends here.
            Err(_) if reader.buffer_position() == before => break,
            Err(_) | Ok(_) => {}
        }
    }
    feeds
}

/// An `<outline>` opened: a feed when it names an address, otherwise a folder its children are
/// filed under until it closes (`empty`: it closes as it opens).
fn opened(tag: &BytesStart<'_>, empty: bool, folders: &mut Vec<String>, feeds: &mut Vec<Outline>) {
    let title = attr(tag, b"title")
        .or_else(|| attr(tag, b"text"))
        .unwrap_or_default();
    if let Some(url) = attr(tag, b"xmlUrl") {
        feeds.push(Outline {
            title: if title.is_empty() { url.clone() } else { title },
            site: attr(tag, b"htmlUrl").unwrap_or_default(),
            url,
            folder: folders.last().cloned().unwrap_or_default(),
        });
        // A feed leaf written with a closing tag is still a leaf: its close pops the frame this
        // pushes, which names no folder, so the folders around it balance.
        if !empty {
            folders.push(folders.last().cloned().unwrap_or_default());
        }
    } else if !empty {
        folders.push(if title.is_empty() {
            "Folder".to_owned()
        } else {
            title
        });
    }
}

/// What `tag` names in the attribute `key`, its escapes read; a value whose escapes do not read
/// (a bare `&` in an address) is kept as written.
fn attr(tag: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    tag.attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .map(|attribute| {
            let written = String::from_utf8_lossy(&attribute.value);
            quick_xml::escape::unescape(&written)
                .map_or_else(|_| written.clone().into_owned(), std::borrow::Cow::into_owned)
        })
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
        out.push_str("    <outline text=\"");
        out.push_str(&name);
        out.push_str("\" title=\"");
        out.push_str(&name);
        out.push_str("\">\n");
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
