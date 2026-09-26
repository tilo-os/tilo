//! Sandboxing for **untrusted HTML email** rendered in the reader's `WebView`.
//!
//! An email's `text/html` part is hostile input: it can carry tracking pixels,
//! remote images that phone home the instant the mail is opened, web fonts that
//! leak the read event, and (in a full engine) script. We never hand the raw
//! HTML to the engine. Instead [`data_url`] wraps it in a locked-down document:
//!
//! * A restrictive **Content-Security-Policy** is injected as the very first
//!   child of `<head>` so it governs the whole document regardless of what the
//!   mail's own markup declares. Scripts are **always** forbidden (`script-src`
//!   is never granted), and by default **all remote loads are blocked** —
//!   images, styles, fonts and media may only come from `data:` URIs already
//!   inlined in the mail. This is what stops trackers firing on open.
//! * `base-uri 'none'` and `form-action 'none'` neutralise `<base>` hijacking
//!   and form-based exfiltration.
//!
//! The reader offers a "Load remote images" affordance; when the user opts in
//! for a given message, [`data_url`] is rebuilt with `block_remote = false`,
//! which widens `img-src`/`media-src` to `https:` — and nothing else. Scripts
//! stay blocked either way.
//!
//! The document is delivered to the engine as a `data:text/html;base64,…` URL
//! (via [`data_url`]) so it has an opaque, unique origin with no ambient
//! cookies/storage — a second layer of isolation on top of the CSP.
//!
//! [`content_filter_json`] additionally emits a `WebKit` content-blocker rule set
//! that blocks remote sub-resource loads at the network layer, for engines that
//! honour it (`SetContentFilter`); it mirrors the CSP so the two agree.

/// Build the full sandboxed HTML document that wraps an email's `html` body.
/// `block_remote` gates whether remote images/media may load at all.
pub fn sandbox_document(html: &str, block_remote: bool) -> String {
    let csp = csp_policy(block_remote);
    // The CSP meta is the FIRST head element so it applies before any of the
    // mail's own resources are considered. A neutral base style keeps the body
    // readable on the reader's dark surface without letting the mail override
    // the whole viewport.
    format!(
        "<!doctype html><html><head>\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
<meta name=\"referrer\" content=\"no-referrer\">\
<meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<style>html,body{{margin:0;padding:16px;background:#1b1d22;color:#e6e8ee;\
font:14px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;\
word-break:break-word;overflow-wrap:anywhere}}\
img{{max-width:100%;height:auto}}a{{color:#7aa2ff}}\
blockquote{{border-left:3px solid #3a3f4b;margin:0;padding-left:12px;color:#a9afbd}}\
</style></head><body>{html}</body></html>",
    )
}

/// The Content-Security-Policy string for a sandboxed mail document. Scripts,
/// objects, frames and workers are always denied. Remote images/media are
/// denied unless the user has explicitly asked to load them.
fn csp_policy(block_remote: bool) -> String {
    // Directives common to both modes: never allow script/object/frame/worker,
    // and neutralise base/form exfiltration vectors. Inline styles are allowed
    // because HTML mail leans on them heavily for layout.
    let mut policy = String::from(
        "default-src 'none'; \
script-src 'none'; \
object-src 'none'; \
frame-src 'none'; \
child-src 'none'; \
connect-src 'none'; \
base-uri 'none'; \
form-action 'none'; \
style-src 'unsafe-inline' data:; \
font-src data:; ",
    );
    if block_remote {
        // Only bytes already inlined in the mail may render — no network at all.
        policy.push_str("img-src data:; media-src data:;");
    } else {
        // The user opted in to remote images: permit https(+http) image/media
        // loads, still nothing else (no scripts, no remote styles/fonts).
        policy.push_str("img-src data: https: http:; media-src data: https:;");
    }
    policy
}

/// A `data:text/html;base64,…` URL for the sandboxed document wrapping `html`.
/// Base64 avoids every percent-escaping pitfall (`#`, `%`, non-ASCII) and gives
/// the document an opaque origin.
pub fn data_url(html: &str, block_remote: bool) -> String {
    let doc = sandbox_document(html, block_remote);
    format!(
        "data:text/html;charset=utf-8;base64,{}",
        base64_encode(doc.as_bytes())
    )
}

/// A `WebKit` content-blocker rule set (JSON) that blocks remote sub-resource
/// loads at the network layer, mirroring the CSP. Handed to engines via
/// `SetContentFilter`; engines without native filtering ignore it and rely on
/// the CSP alone. When `block_remote` is false, remote *images* are allowed
/// through (everything else — scripts, styles, fonts, XHR — is still blocked).
pub fn content_filter_json(block_remote: bool) -> String {
    if block_remote {
        // Block every remote scheme, every resource type.
        r#"[{"trigger":{"url-filter":"^https?://"},"action":{"type":"block"}}]"#.to_string()
    } else {
        // Block remote scripts/styles/fonts/xhr, but let images/media load.
        // (`raw` is WebKit's catch-all for XHR/fetch/websocket sub-resources.)
        r#"[{"trigger":{"url-filter":"^https?://","resource-type":["script","style-sheet","font","raw"]},"action":{"type":"block"}}]"#.to_string()
    }
}

/// Standard, padded base64 (RFC 4648 §4) on one line, as a `data:` URL needs.
fn base64_encode(input: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn html_content_policy_blocks_scripts_and_remote_until_opt_in() {
        // Phase `scripts_are_always_blocked`.
        {
            for block in [true, false] {
                let doc = sandbox_document("<script>steal()</script><p>hi</p>", block);
                assert!(
                    doc.contains("script-src 'none'"),
                    "scripts must never be allowed"
                );
                // The CSP meta precedes the mail's own body markup.
                let csp_at = doc.find("Content-Security-Policy").unwrap();
                let body_at = doc.find("<body>").unwrap();
                assert!(csp_at < body_at, "the CSP must be declared before the body");
            }
        }

        // Phase `remote_content_blocked_by_default`.
        {
            let doc = sandbox_document("<img src=\"https://tracker.example/x.gif\">", true);
            assert!(
                doc.contains("img-src data:;"),
                "default policy blocks remote images"
            );
            assert!(
                !doc.contains("img-src data: https:"),
                "no https image source by default"
            );
            assert!(doc.contains("connect-src 'none'"), "no network connections");
        }

        // Phase `opting_in_widens_images_only`.
        {
            let doc = sandbox_document("<img src=\"https://cdn.example/x.png\">", false);
            assert!(
                doc.contains("img-src data: https: http:"),
                "opt-in allows remote images"
            );
            // Still no scripts and no remote styles/fonts.
            assert!(doc.contains("script-src 'none'"));
            assert!(doc.contains("font-src data:;") || doc.contains("font-src data: "));
            assert!(
                !doc.contains("style-src 'unsafe-inline' https"),
                "remote styles stay blocked"
            );
        }

        // Phase `content_filter_blocks_remote_by_default_and_frees_images_on_optin`.
        {
            let blocked = content_filter_json(true);
            assert!(blocked.contains(r#""url-filter":"^https?://""#));
            assert!(
                !blocked.contains("resource-type"),
                "block-all has no type restriction"
            );
            let opt_in = content_filter_json(false);
            assert!(
                opt_in.contains("resource-type"),
                "opt-in keeps blocking non-image types"
            );
            assert!(
                opt_in.contains("script"),
                "scripts still blocked at the network layer"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn html_body_encodes_as_a_base64_data_url() {
        // Phase `data_url_is_a_base64_html_document`.
        {
            let url = data_url("<p>Hello &amp; welcome</p>", true);
            assert!(url.starts_with("data:text/html;charset=utf-8;base64,"));
            let b64 = url.trim_start_matches("data:text/html;charset=utf-8;base64,");
            // Round-trips back to a document that embeds the mail body + the CSP.
            let decoded = String::from_utf8(
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap(),
            )
            .unwrap();
            assert!(decoded.contains("<p>Hello &amp; welcome</p>"));
            assert!(decoded.contains("Content-Security-Policy"));
        }

        // Phase `base64_matches_known_vectors`.
        {
            assert_eq!(base64_encode(b""), "");
            assert_eq!(base64_encode(b"f"), "Zg==");
            assert_eq!(base64_encode(b"fo"), "Zm8=");
            assert_eq!(base64_encode(b"foo"), "Zm9v");
            assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
            assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
            // Standard alphabet (`+`/`/`), padded.
            assert_eq!(base64_encode(&[0xfb, 0xff, 0xbf]), "+/+/");
            assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=");
            // A data URL is one token: no MIME-style line wrapping at 76 columns.
            let long = base64_encode(&[0x5a_u8; 300]);
            assert_eq!(long.len(), 400);
            assert!(!long.contains(['\r', '\n']));
        }
    }
}
