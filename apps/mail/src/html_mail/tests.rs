use super::*;
use crate::model::{Folder, Message};

const NEWSLETTER: &[u8] = include_bytes!("../../tests/fixtures/html/newsletter.eml");
const GITHUB: &[u8] = include_bytes!("../../tests/fixtures/html/github.eml");
const RECEIPT: &[u8] = include_bytes!("../../tests/fixtures/html/receipt.eml");
const DARK: &[u8] = include_bytes!("../../tests/fixtures/html/dark.eml");
const INLINE: &[u8] = include_bytes!("../../tests/fixtures/html/inline.eml");
const PLAIN: &[u8] = include_bytes!("../../tests/fixtures/html/plain.eml");
const HOSTILE: &[u8] = include_bytes!("../../tests/fixtures/html/hostile.eml");

fn message(raw: &[u8]) -> Message {
    crate::transport::parse_eml(raw, 0, Folder::Inbox).expect("the fixture parses")
}

fn html(raw: &[u8]) -> String {
    message(raw)
        .html_body
        .expect("the fixture has an HTML part")
}

fn policy(remote: bool, dark: bool) -> Policy<'static> {
    Policy {
        remote,
        dark,
        ground: "#1c1c1e",
        ink: "#f2f2f7",
        inline: &[],
    }
}

/// Every address the document makes the engine fetch: image sources and
/// candidates, backgrounds, style sheets, and CSS `url()`s.
fn fetched(document: &str) -> Vec<String> {
    let mut found = Vec::new();
    let lower = document.to_ascii_lowercase();
    for attribute in [" src=\"", " background=\"", " href=\""] {
        let mut from = 0;
        while let Some(at) = lower.get(from..).and_then(|rest| rest.find(attribute)) {
            let start = from + at + attribute.len();
            let end = start
                + lower
                    .get(start..)
                    .and_then(|rest| rest.find('"'))
                    .unwrap_or(0);
            // A link's href is followed, not fetched.
            let tag_start = lower
                .get(..start)
                .and_then(|head| head.rfind('<'))
                .unwrap_or(0);
            let is_anchor = lower
                .get(tag_start..)
                .is_some_and(|tag| tag.starts_with("<a "));
            if !is_anchor {
                found.push(document.get(start..end).unwrap_or_default().to_owned());
            }
            from = end;
        }
    }
    let mut from = 0;
    while let Some(at) = lower.get(from..).and_then(|rest| rest.find(" srcset=\"")) {
        let start = from + at + 9;
        let end = start
            + lower
                .get(start..)
                .and_then(|rest| rest.find('"'))
                .unwrap_or(0);
        for candidate in document.get(start..end).unwrap_or_default().split(", ") {
            found.push(candidate.split(' ').next().unwrap_or_default().to_owned());
        }
        from = end;
    }
    let mut from = 0;
    while let Some(at) = lower.get(from..).and_then(|rest| rest.find("url(")) {
        let start = from + at + 4;
        let end = start
            + lower
                .get(start..)
                .and_then(|rest| rest.find(')'))
                .unwrap_or(0);
        found.push(
            document
                .get(start..end)
                .unwrap_or_default()
                .trim_matches(['"', '\''])
                .to_owned(),
        );
        from = end;
    }
    found
}

fn is_web(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("//")
}

// USER REQUIREMENT (2026-10-22): "Safety (non-negotiable, the security
// boundary): no scripts, no forms submitting, no plugins, no navigation
// inside the pane."
#[test]
fn a_hostile_mail_keeps_no_script_form_plugin_frame_or_navigation() {
    let html = html(HOSTILE);
    for remote in [false, true] {
        let prepared = prepare(&html, &policy(remote, false));
        let document = prepared.document.to_ascii_lowercase();
        for banned in [
            "<script",
            "javascript:",
            "<form",
            "<input",
            "<button",
            "<iframe",
            "<object",
            "<embed",
            "<svg",
            "<math",
            "<video",
            "<audio",
            "<noscript",
            "<template",
            "<base href",
            "refresh",
            "onload",
            "onerror",
            "formaction",
            "srcdoc",
            "ping=",
            "xlink",
            "target=\"_self\" ping",
            "data:text/html",
            "alert(",
            "default-src *",
            "prefetch",
            "preconnect",
        ] {
            assert!(
                !document.contains(banned),
                "remote {remote}: `{banned}` survived:\n{}",
                prepared.document
            );
        }
        // The only policy is the reader's, and it comes first.
        assert_eq!(document.matches("<meta http-equiv").count(), 1);
        let csp = document
            .find("content-security-policy")
            .unwrap_or(usize::MAX);
        assert!(csp < document.find("<style").unwrap_or(0));
        assert!(csp < document.find("<body").unwrap_or(0));
        assert!(document.contains("script-src 'none'"));
        assert!(document.contains("form-action 'none'"));
        assert!(document.contains("object-src 'none'"));
        assert!(document.contains("frame-src 'none'"));
        // The one real link opens outside the message.
        assert_eq!(prepared.links, ["https://evil.example/login"]);
        assert!(
            prepared.document.contains(
                "href=\"https://evil.example/login\" target=\"_blank\" rel=\"noopener noreferrer\""
            ),
            "{}",
            prepared.document
        );
        assert!(document.contains("<base target=\"_blank\">"));
        // Its words stay readable.
        assert!(prepared.document.contains("Your account is on hold."));
        assert!(prepared.document.contains("tricky"));
    }
}

// USER REQUIREMENT (2026-10-22): "Remote content: off by default, one clear
// \"Load images\" control per message, plus \"always for this sender\"."
#[test]
fn nothing_remote_is_fetched_before_the_person_allows_it() {
    for raw in [NEWSLETTER, GITHUB, RECEIPT, DARK, INLINE, HOSTILE] {
        let html = html(raw);
        let blocked = prepare(&html, &policy(false, false));
        for url in fetched(&blocked.document) {
            assert!(
                url.is_empty() || url.starts_with("data:"),
                "fetched before consent: {url}"
            );
        }
        assert!(!blocked.document.contains("@import"));
        assert!(blocked.document.contains("img-src data:;"));
        assert!(blocked.document.contains("font-src data:;"));
        assert!(
            blocked
                .document
                .contains("style-src 'unsafe-inline' data:;")
        );
    }
    // The newsletter's images, its font and its tracking pixel are counted,
    // so the reader can offer them.
    let newsletter = html(NEWSLETTER);
    let blocked = prepare(&newsletter, &policy(false, false));
    assert!(blocked.remote >= 3, "{}", blocked.remote);
    let allowed = prepare(&newsletter, &policy(true, false));
    assert_eq!(allowed.remote, blocked.remote);
    assert!(fetched(&allowed.document).iter().any(|url| is_web(url)));
    assert!(allowed.document.contains("img-src data: https: http:"));
    // Allowed, it is still no more than images, fonts and styles.
    assert!(allowed.document.contains("script-src 'none'"));
    assert!(allowed.document.contains("connect-src 'none'"));
    // A plain mail has nothing to allow.
    assert_eq!(prepare("<p>Hi</p>", &policy(false, false)).remote, 0);
}

#[test]
fn the_request_filter_refuses_every_web_request_until_allowed() {
    let blocked = filter_rules(false);
    let lines: Vec<&str> = blocked.lines().collect();
    for scheme in ["http", "https"] {
        assert!(lines.contains(&format!("|{scheme}://").as_str()));
        assert!(lines.contains(&format!("|{scheme}://$document").as_str()));
        assert!(lines.contains(&format!("|{scheme}://$subdocument").as_str()));
    }
    let allowed = filter_rules(true);
    let lines: Vec<&str> = allowed.lines().collect();
    assert!(!lines.contains(&"|https://"));
    assert!(lines.contains(&"|https://$script"));
    assert!(lines.contains(&"|https://$xmlhttprequest"));
    assert!(lines.contains(&"|https://$document"));
    for kind in ["image", "font", "stylesheet", "media"] {
        assert!(!allowed.contains(kind), "{kind} must load once allowed");
    }
}

#[test]
fn escaped_and_disguised_addresses_are_read_as_the_engine_reads_them() {
    let cases = [
        // Entities and whitespace inside a scheme.
        "<a href=\"java&#x09;script:alert(1)\">a</a>",
        "<a href=\"javascript&colon;alert(1)\">a</a>",
        "<a href=\"jav&#97;script:alert(1)\">a</a>",
        "<a href=\"\njavascript:alert(1)\">a</a>",
        "<a href=\"vbscript:msgbox(1)\">a</a>",
        "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">a</a>",
        "<a href=\"file:///etc/passwd\">a</a>",
    ];
    for case in cases {
        let prepared = prepare(case, &policy(false, false));
        assert!(prepared.links.is_empty(), "{case}: {:?}", prepared.links);
        assert!(!prepared.document.contains("href="), "{case}");
    }
    let fetches = [
        "<img src=\"htt&#112;s://t.example/a.gif\">",
        "<img src=\"//t.example/a.gif\">",
        "<img src=\" https://t.example/a.gif \">",
        "<div style=\"background:u\\72l(https://t.example/a.gif)\">x</div>",
        "<div style=\"background:url(&quot;https://t.example/a.gif&quot;)\">x</div>",
        "<div style=\"background:&#117;rl(https://t.example/a.gif)\">x</div>",
        "<div style=\"background-image:image-set('https://t.example/a.gif' 1x)\">x</div>",
        "<style>@\\69mport 'https://t.example/a.css';</style>",
        "<style>@import url(https://t.example/a.css);</style>",
        "<style>p{background:URL( 'https://t.example/a.gif' )}</style>",
        "<style>p{background:url(https://t.example/a.gif)}</style><p>x</p>",
        "<table background=\"https://t.example/a.gif\"><tr><td>x</td></tr></table>",
        "<img srcset=\"https://t.example/a.gif 2x, data:image/gif;base64,R0lGOD 1x\">",
        "<link rel=\"stylesheet\" href=\"https://t.example/a.css\">",
    ];
    for case in fetches {
        let prepared = prepare(case, &policy(false, false));
        assert!(
            !prepared.document.contains("t.example"),
            "{case} kept a remote address:\n{}",
            prepared.document
        );
        assert_eq!(prepared.remote, 1, "{case} is one remote reference");
    }
    // Encoded twice, the engine reads the text an entity, not a URL.
    let twice = prepare(
        "<div style=\"background:&amp;#117;rl(https://t.example/a.gif)\">x</div>",
        &policy(false, false),
    );
    assert!(
        twice.document.contains("&amp;#117;rl("),
        "{}",
        twice.document
    );
}

#[test]
fn markup_cannot_escape_the_text_or_attribute_it_is_written_in() {
    let cases = [
        "<p title='a\" onmouseover=\"alert(1)'>x</p>",
        "<p title=\"a\" onmouseover=alert(1)>x</p>",
        "<p>a</p><scr<script>ipt>alert(1)</script>",
        "<style>p{content:\"</style><script>alert(1)</script>\"}</style>",
        "<!-- <script>alert(1)</script> -->",
        "<!--><script>alert(1)</script>-->",
        "<p>x<plaintext><script>alert(1)</script>",
        "<textarea><script>alert(1)</script></textarea>",
        "<title><script>alert(1)</script></title>",
        "<noscript><p title=\"</noscript><script>alert(1)</script>\"></noscript>",
        "<svg/><script>alert(1)</script>",
        "<p><svg><p><style><img src=x onerror=alert(1)></style></svg>",
        "<img src=x onerror=alert(1)//",
        "<a href=\"https://ok.example\" onclick=\"alert(1)\">ok</a>",
    ];
    for case in cases {
        let prepared = prepare(case, &policy(false, false));
        let lower = prepared.document.to_ascii_lowercase();
        assert!(!lower.contains("<script"), "{case}:\n{}", prepared.document);
        assert!(!lower.contains(" on"), "{case}:\n{}", prepared.document);
        assert!(
            !lower.contains("<img src=x"),
            "{case}:\n{}",
            prepared.document
        );
    }
}

// USER REQUIREMENT (2026-10-22): "Inline images: cid: and data: always load."
#[test]
fn inline_parts_and_data_images_always_show() {
    let message = message(INLINE);
    let html = message.html_body.clone().expect("an HTML part");
    let inline: Vec<InlinePart> = message
        .attachments
        .iter()
        .filter_map(|attachment| {
            Some(InlinePart {
                id: attachment.inline_id.clone()?,
                mime: attachment.mime.clone(),
                bytes: attachment.content.clone(),
            })
        })
        .collect();
    assert_eq!(inline.len(), 1, "the logo is an inline part");
    assert_eq!(
        inline.first().map(|part| part.id.as_str()),
        Some("logo-v3@studio-lima.example")
    );
    let prepared = prepare(
        &html,
        &Policy {
            inline: &inline,
            ..policy(false, false)
        },
    );
    assert!(!prepared.document.contains("cid:"));
    let images: Vec<String> = fetched(&prepared.document);
    assert_eq!(
        images
            .iter()
            .filter(|url| url.starts_with("data:image/png;base64,"))
            .count(),
        2,
        "{images:?}"
    );
    assert_eq!(prepared.remote, 0);
    // The logo is part of the message, not a file attached to it.
    assert!(message.shown_attachments().next().is_none());
    // An inline part the HTML does not show is still offered as a file.
    let unreferenced = String::from_utf8_lossy(INLINE).replace("cid:logo-v3", "cid:other");
    let unreferenced = self::message(unreferenced.as_bytes());
    assert_eq!(unreferenced.shown_attachments().count(), 1);
    // A missing part is no image at all, never a fetch.
    let missing = prepare("<img src=\"cid:gone@x\">", &policy(false, false));
    assert!(!missing.document.contains("src="));
}

// USER REQUIREMENT (2026-10-22): "Dark mode: respect a mail's own
// color-scheme and dark media queries; otherwise keep the mail's colours on a
// light sheet, never inverted."
#[test]
fn a_mail_is_dark_only_when_it_says_it_can_be() {
    let dark = html(DARK);
    let in_dark = prepare(&dark, &policy(false, true));
    assert!(in_dark.dark);
    assert!(
        in_dark
            .document
            .contains("<meta name=\"color-scheme\" content=\"dark\">")
    );
    assert!(
        in_dark
            .document
            .contains("html{background:#1c1c1e;color:#f2f2f7}")
    );
    assert!(!in_dark.document.contains("prefers-color-scheme"));
    assert!(
        in_dark.document.contains("min-width:0px"),
        "the dark rules hold"
    );
    let in_light = prepare(&dark, &policy(false, false));
    assert!(!in_light.dark);
    assert!(
        in_light.document.contains("max-width:-1px"),
        "the dark rules do not hold"
    );
    for raw in [NEWSLETTER, GITHUB, RECEIPT, INLINE] {
        let prepared = prepare(&html(raw), &policy(false, true));
        assert!(!prepared.dark, "a mail without a dark form stays light");
        assert!(
            prepared.document.contains(
                ":root{color-scheme:light!important}html{background:#ffffff;color:#000000}"
            )
        );
        assert!(!prepared.document.contains("invert"));
    }
}

// USER REQUIREMENT (2026-10-22): "Fallbacks: multipart/alternative prefers
// HTML and keeps text available; plain text renders as nicely set text with
// links detected."
#[test]
fn multipart_alternative_prefers_html_and_keeps_its_text() {
    let newsletter = message(NEWSLETTER);
    assert!(newsletter.html_body.is_some());
    assert!(
        newsletter.body.contains("WEEKLY HARVEST"),
        "{}",
        newsletter.body
    );
    assert!(!newsletter.body.contains('<'), "the text part, not markup");
    let github = message(GITHUB);
    assert!(github.html_body.is_some());
    assert!(!github.body.trim().is_empty());
    let receipt = message(RECEIPT);
    assert!(receipt.html_body.is_some(), "an HTML-only mail is HTML");
    let plain = message(PLAIN);
    assert!(
        plain.html_body.is_none(),
        "a text mail never goes to the engine"
    );
    assert!(plain.body.contains("https://"));
}

#[test]
fn a_document_keeps_the_doctype_it_was_written_for_and_loads_as_data() {
    let standards = prepare(
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"x\"><p>a</p>",
        &policy(false, false),
    );
    assert!(standards.document.starts_with("<!DOCTYPE html PUBLIC"));
    let quirks = prepare("<p>a</p>", &policy(false, false));
    assert!(quirks.document.starts_with("<html>"));
    let late = prepare("<p>a</p><!doctype html>", &policy(false, false));
    assert!(late.document.starts_with("<html>"));

    let url = data_url("<p>Grüße & #1 100%</p>").expect("small enough");
    let encoded = url
        .strip_prefix("data:text/html;charset=utf-8;base64,")
        .expect("a base64 HTML document");
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .expect("valid base64");
    assert_eq!(decoded, "<p>Grüße & #1 100%</p>".as_bytes());
    assert!(
        data_url(&"x".repeat(2 * 1024 * 1024)).is_none(),
        "too large for the engine"
    );
}

#[test]
fn links_are_listed_once_in_reading_order() {
    let links = links(
        "<a href=\"https://b.example\">b</a><a href=\"mailto:a@example.com\">a</a>\
<a href=\"https://b.example\">again</a><a href=\"#top\">top</a><a href=\"//c.example/x\">c</a>",
    );
    assert_eq!(
        links,
        [
            "https://b.example",
            "mailto:a@example.com",
            "https://c.example/x"
        ]
    );
    let fragment = prepare("<a href=\"#top\">top</a>", &policy(false, false));
    assert!(fragment.document.contains("href=\"#top\" target=\"_self\""));
}

#[test]
fn plain_text_names_its_links() {
    let text = "See https://harvest.example/112/elm-street. Or www.harvest.example/swaps, \
(details at https://x.example/a_(b)) and write to ana@studio-lima.example!\n\
Not links: a@b, http://, www.x, sam@localhost.";
    let found: Vec<String> = text_links(text).into_iter().map(|(_, url)| url).collect();
    assert_eq!(
        found,
        [
            "https://harvest.example/112/elm-street",
            "https://www.harvest.example/swaps",
            "https://x.example/a_(b)",
            "mailto:ana@studio-lima.example",
        ]
    );
    for (range, url) in text_links(text) {
        let shown = text.get(range).unwrap_or_default();
        assert!(
            [
                shown.to_owned(),
                format!("https://{shown}"),
                format!("mailto:{shown}")
            ]
            .contains(&url),
            "{shown} -> {url}"
        );
    }
    let plain = message(PLAIN);
    let links = message_links(&plain);
    assert!(!links.is_empty(), "{}", plain.body);
    assert!(links.iter().all(|url| url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("mailto:")));
}
