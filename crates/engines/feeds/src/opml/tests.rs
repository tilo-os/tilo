//! OPML as people's own exports write it: folders in folders, either quote, and a file that is
//! not one at all.

use super::*;

#[test]
fn a_file_lists_its_feeds_in_the_folders_around_them() {
    let sample = r#"<?xml version="1.0"?>
    <opml version="2.0"><head><title>subs</title></head><body>
      <outline text="Tech" title="Tech">
        <outline type="rss" text="Lobsters" title="Lobsters" xmlUrl="https://lobste.rs/rss" htmlUrl="https://lobste.rs"/>
        <outline type="atom" text="Hacker News" xmlUrl="https://hnrss.org/frontpage" htmlUrl="https://news.ycombinator.com"/>
      </outline>
      <outline type="rss" text="Top Level" xmlUrl="https://example.com/feed" htmlUrl="https://example.com"/>
    </body></opml>"#;
    let feeds = parse(sample);
    assert_eq!(feeds.len(), 3);
    assert_eq!(feeds[0].title, "Lobsters");
    assert_eq!(feeds[0].url, "https://lobste.rs/rss");
    assert_eq!(feeds[0].site, "https://lobste.rs");
    assert_eq!(feeds[0].folder, "Tech");
    assert_eq!(feeds[1].folder, "Tech");
    assert_eq!(feeds[2].folder, "");
}

#[test]
fn the_edges_of_what_people_export_read_as_they_look() {
    // An empty file, a file without outlines and one that is no XML at all list nothing.
    assert!(parse("").is_empty());
    assert!(parse(r#"<opml version="2.0"><head/><body></body></opml>"#).is_empty());
    assert!(parse("a < b and outlines > none").is_empty());
    // Single quotes, no title (the address names it), and a title over the text.
    let one = parse(
        "<outline text='One' xmlUrl='https://one.test/feed' htmlUrl='https://one.test'/>",
    );
    assert_eq!(one[0].title, "One");
    assert_eq!(one[0].site, "https://one.test");
    let bare = parse(r#"<outline xmlUrl="https://bare.test/feed"/>"#);
    assert_eq!(bare[0].title, "https://bare.test/feed");
    let titled = parse(r#"<outline text="Text" title="Title" xmlUrl="https://t.test/feed"/>"#);
    assert_eq!(titled[0].title, "Title");
    // An attribute that only ends in the name is not it, and an ampersand left bare stays.
    let near = parse(r#"<outline text="X" myxmlUrl="https://wrong.test" xmlUrl="https://right.test/feed"/>"#);
    assert_eq!(near[0].url, "https://right.test/feed");
    let amp = parse(r#"<outline text="Q" xmlUrl="https://q.test/feed?a=1&b=2"/>"#);
    assert_eq!(amp[0].url, "https://q.test/feed?a=1&b=2");
    // A folder without a name is named, an empty folder files nothing, and a stray close is no
    // folder.
    let unnamed = parse(r#"<outline><outline text="In" xmlUrl="https://in.test/feed"/></outline>"#);
    assert_eq!(unnamed[0].folder, "Folder");
    let empty = parse(r#"<outline text="Empty"/><outline text="Feed" xmlUrl="https://feed.test/feed"/>"#);
    assert_eq!(empty[0].folder, "");
    let stray = parse(r#"</outline><outline text="Feed" xmlUrl="https://feed.test/feed"/>"#);
    assert_eq!(stray[0].folder, "");
    // Folders in folders: a feed is in the innermost one around it.
    let nested = parse(
        r#"<outline text="Outer"><outline text="Inner">
             <outline text="Deep" xmlUrl="https://deep.test/feed"/></outline>
             <outline text="Shallow" xmlUrl="https://shallow.test/feed"/></outline>
           <outline text="Top" xmlUrl="https://top.test/feed"/>"#,
    );
    assert_eq!(nested[0].folder, "Inner");
    assert_eq!(nested[1].folder, "Outer");
    assert_eq!(nested[2].folder, "");
    // A feed written with a closing tag ends there.
    let closed = parse(
        r#"<outline text="F"><outline text="Leaf" xmlUrl="https://leaf.test/feed"></outline>
             <outline text="Next" xmlUrl="https://next.test/feed"/></outline>
           <outline text="Top" xmlUrl="https://top.test/feed"/>"#,
    );
    assert_eq!(closed[0].folder, "F");
    assert_eq!(closed[1].folder, "F");
    assert_eq!(closed[2].folder, "");
}

#[test]
fn what_is_written_reads_back_the_same() {
    let feeds = vec![
        OutLine {
            title: "A & B".to_owned(),
            url: "https://x.com/feed?a=1&b=2".to_owned(),
            site: "https://x.com".to_owned(),
            format: "rss".to_owned(),
            folder: "Reading".to_owned(),
        },
        OutLine {
            title: "Top".to_owned(),
            url: "https://top.test/feed".to_owned(),
            site: String::new(),
            format: "atom".to_owned(),
            folder: String::new(),
        },
    ];
    let written = export(&feeds);
    let again = parse(&written);
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].title, "A & B");
    assert_eq!(again[0].url, "https://x.com/feed?a=1&b=2");
    assert_eq!(again[0].folder, "Reading");
    assert_eq!(again[1].title, "Top");
    assert_eq!(again[1].folder, "");
    assert!(written.contains(r#"type="atom""#), "{written}");
}
