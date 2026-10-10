//! Minimal, dependency-free OPML 2.0 import/export for subscriptions.
//!
//! OPML is a tiny XML dialect: feeds are `<outline type="rss"
//! xmlUrl="..."/>` leaves, optionally nested inside `<outline text="...">`
//! folder groups. The app supports one nesting level with a small,
//! dependency-free scanner.

use crate::model::{Feed, FeedKind, Folder, Store};
use uuid::Uuid;

/// A parsed subscription line from an OPML file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpmlFeed {
    pub title: String,
    pub xml_url: String,
    pub html_url: String,
    pub folder: Option<String>,
}

/// Parse OPML text into a flat list of feeds (with their folder name, if
/// nested). Tolerant of attribute order, single/double quotes, and missing
/// optional attributes.
pub fn parse(text: &str) -> Vec<OpmlFeed> {
    let mut feeds = Vec::new();
    let mut folder_stack: Vec<String> = Vec::new();
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
        // Closing tag?
        if rest.starts_with("</outline") {
            folder_stack.pop();
            i += "</outline".len();
            continue;
        }
        if !rest.starts_with("<outline") {
            i += 1;
            continue;
        }
        // Find the end of the tag.
        let Some(rel_end) = rest.find('>') else {
            break;
        };
        let Some(tag) = text.get(i..=i + rel_end) else {
            break;
        };
        let self_closing = tag.trim_end().ends_with("/>");

        let xml_url = attr(tag, "xmlUrl");
        let title = attr(tag, "title")
            .or_else(|| attr(tag, "text"))
            .unwrap_or_default();
        let html_url = attr(tag, "htmlUrl").unwrap_or_default();

        if let Some(xml_url) = xml_url {
            feeds.push(OpmlFeed {
                title: if title.is_empty() {
                    xml_url.clone()
                } else {
                    title
                },
                xml_url,
                html_url,
                folder: folder_stack.last().cloned(),
            });
            // A feed leaf may be written non-self-closing in sloppy OPML;
            // only push a folder frame for a genuine container (no xmlUrl).
            if !self_closing {
                // Treat as a leaf regardless — pop is balanced by the
                // matching </outline> handler, which pops once.
                folder_stack.push(String::new());
            }
        } else if !self_closing {
            // Folder container.
            folder_stack.push(if title.is_empty() {
                "Folder".to_string()
            } else {
                title
            });
        }
        i += rel_end + 1;
    }

    feeds
}

/// Extract an attribute value from a tag string (handles `"` and `'`).
fn attr(tag: &str, key: &str) -> Option<String> {
    let mut search = 0;
    while let Some(tail) = tag.get(search..) {
        let Some(rel) = tail.find(key) else {
            break;
        };
        let pos = search + rel;
        // Ensure it's a standalone attribute (preceded by whitespace).
        let prev_ok = pos == 0
            || tag
                .as_bytes()
                .get(pos - 1)
                .is_some_and(u8::is_ascii_whitespace);
        let after = tag.get(pos + key.len()..)?;
        let after_trim = after.trim_start();
        if prev_ok && after_trim.starts_with('=') {
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

/// Serialise the store's subscriptions to OPML 2.0, grouping feeds under
/// their folder outlines.
pub fn export(store: &Store) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<opml version=\"2.0\">\n  <head>\n    <title>Feeds subscriptions</title>\n  </head>\n  <body>\n");

    // Folders first, each with its feeds nested.
    for folder in store.folders_sorted() {
        let name = quick_xml::escape::escape(&folder.name);
        let _ = writeln!(out, "    <outline text=\"{name}\" title=\"{name}\">");
        for feed in store.feeds_in_folder(Some(folder.id)) {
            out.push_str(&feed_outline(feed, 6));
        }
        out.push_str("    </outline>\n");
    }
    // Uncategorised feeds at the top level.
    for feed in store.feeds_in_folder(None) {
        out.push_str(&feed_outline(feed, 4));
    }

    out.push_str("  </body>\n</opml>\n");
    out
}

fn feed_outline(feed: &Feed, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let kind = match feed.kind {
        FeedKind::Atom => "atom",
        _ => "rss",
    };
    format!(
        "{pad}<outline type=\"{kind}\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\" htmlUrl=\"{2}\"/>\n",
        quick_xml::escape::escape(&feed.title),
        quick_xml::escape::escape(&feed.url),
        quick_xml::escape::escape(&feed.site_url),
    )
}

/// A small representative OPML document (a folder with two nested feeds plus a
/// top-level feed) shared by `--shots` staging and the import tests, so the
/// screenshot and the tests exercise the same parse path.
pub fn sample_opml() -> &'static str {
    // Feeds deliberately distinct from the bundled sample subscriptions so an
    // import genuinely adds every one (nothing is deduped away).
    r#"<?xml version="1.0"?>
<opml version="2.0">
  <head><title>subscriptions</title></head>
  <body>
    <outline text="Reading" title="Reading">
      <outline type="rss" text="LWN" title="LWN" xmlUrl="https://lwn.net/headlines/rss" htmlUrl="https://lwn.net"/>
      <outline type="atom" text="Simon Willison" title="Simon Willison" xmlUrl="https://simonwillison.net/atom/everything/" htmlUrl="https://simonwillison.net"/>
    </outline>
    <outline type="rss" text="Julia Evans" title="Julia Evans" xmlUrl="https://jvns.ca/atom.xml" htmlUrl="https://jvns.ca"/>
  </body>
</opml>"#
}

/// Merge parsed OPML feeds into a store, creating folders as needed.
/// Returns the number of feeds added.
pub fn import_into(store: &mut Store, parsed: &[OpmlFeed]) -> usize {
    let mut added = 0;
    for of in parsed {
        // Skip if we already subscribe to this URL.
        if store.feeds.iter().any(|f| f.url == of.xml_url) {
            continue;
        }
        let folder_id = of.folder.as_ref().map(|name| {
            if let Some(existing) = store.folders.iter().find(|f| &f.name == name) {
                existing.id
            } else {
                let id = Uuid::new_v4();
                let order = store.folders.len() as u32;
                store.upsert_folder(Folder {
                    id,
                    name: name.clone(),
                    order,
                    collapsed: false,
                });
                id
            }
        });
        let order = store.next_feed_order(folder_id);
        store.upsert_feed(Feed {
            id: Uuid::new_v4(),
            title: of.title.clone(),
            url: of.xml_url.clone(),
            site_url: of.html_url.clone(),
            folder: folder_id,
            order,
            kind: FeedKind::Rss,
            certificate_pin: None,
        });
        added += 1;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<opml version="2.0">
  <head><title>subs</title></head>
  <body>
    <outline text="Tech" title="Tech">
      <outline type="rss" text="Lobsters" title="Lobsters" xmlUrl="https://lobste.rs/rss" htmlUrl="https://lobste.rs"/>
      <outline type="atom" text="Hacker News" xmlUrl="https://hnrss.org/frontpage" htmlUrl="https://news.ycombinator.com"/>
    </outline>
    <outline type="rss" text="Top Level" xmlUrl="https://example.com/feed" htmlUrl="https://example.com"/>
  </body>
</opml>"#;

    /// OPML parses nested and flat outlines, imports with folders and dedup, round-trips export and handles escaped ampersands.
    #[test]
    fn opml_parses_imports_and_round_trips() {
        // --- phase: parses_nested_and_flat ---
        {
            let feeds = parse(SAMPLE);
            assert_eq!(feeds.len(), 3);
            assert_eq!(feeds[0].title, "Lobsters");
            assert_eq!(feeds[0].xml_url, "https://lobste.rs/rss");
            assert_eq!(feeds[0].folder.as_deref(), Some("Tech"));
            assert_eq!(feeds[1].folder.as_deref(), Some("Tech"));
            assert_eq!(feeds[2].folder, None);
        }

        // --- phase: import_creates_folders_and_dedups ---
        {
            let mut store = Store::new();
            let feeds = parse(SAMPLE);
            let n = import_into(&mut store, &feeds);
            assert_eq!(n, 3);
            assert_eq!(store.folders.len(), 1);
            assert_eq!(store.feeds.len(), 3);
            // Re-import: nothing new.
            let n2 = import_into(&mut store, &feeds);
            assert_eq!(n2, 0);
        }

        // --- phase: export_round_trips_through_parse ---
        {
            let mut store = Store::new();
            import_into(&mut store, &parse(SAMPLE));
            let xml = export(&store);
            let reparsed = parse(&xml);
            assert_eq!(reparsed.len(), 3);
            let titles: Vec<_> = reparsed.iter().map(|f| f.title.clone()).collect();
            assert!(titles.contains(&"Lobsters".to_string()));
            assert!(titles.contains(&"Top Level".to_string()));
        }

        // --- phase: handles_escaped_ampersand ---
        {
            let xml = r#"<outline text="A &amp; B" xmlUrl="https://x.com/feed?a=1&amp;b=2"/>"#;
            let feeds = parse(xml);
            assert_eq!(feeds.len(), 1);
            assert_eq!(feeds[0].title, "A & B");
            assert_eq!(feeds[0].xml_url, "https://x.com/feed?a=1&b=2");
        }
    }
}
