//! Reading what a site sent: an RSS item's facts and body, an Atom entry's web page, a capsule's
//! one article, and the Markdown an article's HTML becomes.

use super::*;

#[test]
fn a_feed_gives_its_title_and_its_items_facts_and_body() {
    let rss = r#"<?xml version="1.0"?>
    <rss version="2.0"><channel>
      <title>Test Feed</title>
      <item><title>First</title><link>https://x.com/1</link>
        <description>&lt;p&gt;Hello &amp; welcome&lt;/p&gt;</description>
        <pubDate>Mon, 30 Jun 2025 12:00:00 GMT</pubDate></item>
    </channel></rss>"#;
    let found = feed(rss.as_bytes(), "https://x.com/feed").expect("a feed");
    assert_eq!(found.title, "Test Feed");
    assert_eq!(found.articles.len(), 1);
    let article = &found.articles[0];
    assert_eq!(article.title, "First");
    assert_eq!(article.url, "https://x.com/1");
    assert!(article.published > 0, "it was published");
    assert!(article.summary.contains("Hello & welcome"), "{}", article.summary);
    assert_eq!(article.body, "Hello & welcome");
}

#[test]
fn an_atom_entry_links_its_web_page_not_its_self_or_its_enclosure() {
    let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom">
      <id>https://example.org/feed</id><title>News</title>
      <updated>2025-06-30T12:00:00Z</updated>
      <entry><id>https://example.org/posts/1</id><title>First</title>
        <updated>2025-06-30T12:00:00Z</updated>
        <link rel="self" type="application/atom+xml" href="https://example.org/api/1"/>
        <link rel="enclosure" type="audio/mpeg" href="https://example.org/audio/1"/>
        <link rel="alternate" type="application/json" href="https://example.org/json/1"/>
        <link rel="alternate" type="text/html" href="https://example.org/posts/1"/>
      </entry>
      <entry><id>https://example.org/posts/2</id><title>Second</title>
        <updated>2025-06-30T12:00:00Z</updated>
        <link rel="self" href="https://example.org/api/2"/>
        <link href="https://example.org/posts/2"/>
      </entry>
    </feed>"#;
    let found = feed(atom.as_bytes(), "https://example.org/feed").expect("an atom feed");
    assert_eq!(found.articles[0].url, "https://example.org/posts/1");
    assert_eq!(found.articles[1].url, "https://example.org/posts/2");
}

#[test]
fn html_becomes_markdown_with_its_headings_lists_and_code() {
    let html = "<h2>Title</h2><p>First para.</p><ul><li>One</li><li>Two</li></ul>\
                <blockquote>Said</blockquote><pre>fn main() {\n    work();\n}</pre>\
                <p>caf&#233; &#x41;</p><hr>";
    assert_eq!(
        markdown(html),
        "## Title\n\nFirst para.\n\n- One\n\n- Two\n\n> Said\n\n\
         ```\nfn main() {\n    work();\n}\n```\n\ncafé A\n\n---"
    );
}

#[test]
fn a_line_that_would_start_a_block_is_escaped() {
    assert_eq!(markdown("<p># not a heading</p>"), "\\# not a heading");
    assert_eq!(markdown("<p>- not an item</p>"), "\\- not an item");
    assert_eq!(markdown("<p>a * b</p>"), "a \\* b");
}

#[test]
fn a_capsule_is_one_article_with_its_first_heading_as_its_title() {
    let capsule = "# A capsule\n## Sub\nA paragraph.\n* item\n=> gemini://example.org Capsule\n\
                   => gemini://bare.org\n```\ncode\n```";
    let found = gemtext(capsule, "gemini://example.org/");
    assert_eq!(found.articles.len(), 1);
    let article = &found.articles[0];
    assert_eq!(article.title, "A capsule");
    assert_eq!(article.url, "gemini://example.org/");
    assert_eq!(
        article.body,
        "# A capsule\n\n## Sub\n\nA paragraph.\n\n- item\n\n\
         [Capsule](gemini://example.org)\n\n[gemini://bare.org](gemini://bare.org)\n\n\
         ```\ncode\n```"
    );
}

#[test]
fn bytes_that_are_no_feed_are_refused_as_such() {
    let failed = feed(b"<html><body>not a feed</body></html>", "https://x.test/").unwrap_err();
    assert_eq!(failed.reason, crate::Reason::NotAFeed);
}
