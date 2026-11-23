//! The pages a person sees at the server: their grants, each revocable, the
//! key they just approved, and short messages that always carry the way on.
//! Approving an agent happens in the person's own Tilo, which holds what
//! signs for them.
//!
//! Plain server-rendered HTML with no script, so a decision is the form the
//! person submits and nothing else on the page can make it. The card is the
//! one the person's sign-in page draws (the desktop's own tokens, type and
//! button styles), so every page here looks like the product.

use crate::store::{Grant, GrantKind, Scope};
use std::fmt::Write as _;

const STYLE: &str = "
:root{color-scheme:dark;--ground:#121214;--card:#1c1d21;--line:#2c2e34;--shadow:rgb(0 0 0/45%);--ink:#ddd;--heading:#f2f2f4;--quiet:#8f8f99;--text:#b9b9c0;--value:#e6e6e9;--button-line:#34363d;--button:#d4d4d8;--button-hover:#26282e;--primary-bg:#f2f2f4;--primary-ink:#121214;--primary-hover:#fff;--field-bg:#17181c;--danger:#f2b8b5;--focus:#9ab4ff}
@media (prefers-color-scheme:light){:root{color-scheme:light;--ground:#f5f5f7;--card:#fff;--line:#e2e3e8;--shadow:rgb(15 18 32/12%);--ink:#26272b;--heading:#121214;--quiet:#6a6b73;--text:#55565e;--value:#1b1c20;--button-line:#d5d6dc;--button:#3a3b42;--button-hover:#f0f1f4;--primary-bg:#1b1c20;--primary-ink:#fff;--primary-hover:#000;--field-bg:#fafafb;--danger:#b3261e;--focus:#2a55d6}}
*{box-sizing:border-box}
html,body{margin:0}
body{min-height:100vh;background:var(--ground);color:var(--ink);font:14px/1.5 system-ui,sans-serif;display:grid;place-items:center;padding:16px}
main{display:grid;gap:18px;width:min(560px,100%);padding:26px;border-radius:14px;background:var(--card);border:1px solid var(--line);box-shadow:0 12px 40px var(--shadow)}
svg.mark{display:block}
h1{margin:0;font-size:19px;font-weight:600;line-height:1.35;color:var(--heading);text-wrap:balance;overflow-wrap:anywhere}
h2{margin:4px 0 0;font-size:15px;font-weight:600;color:var(--heading)}
p{margin:0;color:var(--text)}
.quiet{color:var(--quiet);font-size:13px}
p code{font:13px/1.5 ui-monospace,SFMono-Regular,monospace;color:var(--value)}
code.key{display:block;padding:10px 12px;border-radius:8px;border:1px solid var(--line);background:var(--field-bg);color:var(--value);font:13px/1.5 ui-monospace,SFMono-Regular,monospace;word-break:break-all}
ul.grants{list-style:none;margin:0;padding:0;display:grid;gap:10px}
ul.grants li{display:flex;justify-content:space-between;align-items:center;gap:16px;padding:12px 14px;border-radius:10px;background:var(--field-bg);border:1px solid var(--line)}
ul.grants li span{min-width:0;overflow-wrap:anywhere}
ul.grants strong{color:var(--heading);font-weight:600}
ul.grants ul.facts{list-style:none;margin:2px 0 0;padding:0;display:flex;flex-wrap:wrap;column-gap:14px;row-gap:0;color:var(--quiet);font-size:13px}
ul.grants ul.facts li{display:block;padding:0;border:0;background:none;border-radius:0}
.actions{display:flex;flex-direction:row-reverse;justify-content:flex-start;gap:8px;flex-wrap:wrap}
button,a.button{font:inherit;padding:8px 14px;border-radius:8px;border:1px solid var(--button-line);background:transparent;color:var(--button);cursor:pointer;text-decoration:none;display:inline-block}
button:hover,a.button:hover{background:var(--button-hover)}
button.primary,a.button.primary{border-color:transparent;background:var(--primary-bg);color:var(--primary-ink);font-weight:500}
button.primary:hover,a.button.primary:hover{background:var(--primary-hover)}
button.danger{color:var(--danger)}
button:focus-visible,a:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
form{margin:0}
form.new{display:grid;gap:8px}
form.new input,form.new select{width:100%;font:inherit;padding:8px 10px;border-radius:8px;border:1px solid var(--line);background:var(--field-bg);color:var(--value)}
form.new label.browser-pages{display:flex;align-items:center;gap:9px;color:var(--value)}
form.new label.browser-pages input{width:15px;height:15px;margin:0;accent-color:var(--primary-bg)}
";

/// The product's mark, in its own light and dark colours, at the top of
/// every card.
const MARK: &str = "<svg class=\"mark\" aria-hidden=\"true\" viewBox=\"0 0 100 100\" width=\"40\" height=\"40\">\
<style>.w{fill:#9fa6e0}.b{fill:#2a3fd1}@media (prefers-color-scheme:dark){.w{fill:#5b64b8}.b{fill:#a8b0f0}}</style>\
<path class=\"w\" d=\"M10.98 31.85Q10.98 27.97 14.4 26.12L20.26 22.97Q24.84 20.51 24.84 25.71V74.29Q24.84 79.49 20.26 77.03L14.4 73.88Q10.98 72.03 10.98 68.15Z\"/>\
<path class=\"w\" d=\"M89.02 31.85Q89.02 27.97 85.6 26.12L79.74 22.97Q75.16 20.51 75.16 25.71V74.29Q75.16 79.49 79.74 77.03L85.6 73.88Q89.02 72.03 89.02 68.15Z\"/>\
<rect class=\"b\" x=\"29\" y=\"16\" width=\"42\" height=\"68\" rx=\"6\"/></svg>";

/// The tab's icon, the mark on the desktop's own ground.
const ICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E%3Cpath d='M10.98 31.85 Q10.979892448752274 27.96648782251284 14.4 26.12 L20.26 22.97 Q24.843373493975903 20.50602409638554 24.84 25.71 L24.84 74.29 Q24.843373493975903 79.49397590361446 20.26 77.03 L14.4 73.88 Q10.979892448752274 72.03351217748715 10.98 68.15 Z' fill='%239fa6e0'/%3E%3Cpath d='M89.02 31.85 Q89.02010755124772 27.96648782251284 85.6 26.12 L79.74 22.97 Q75.1566265060241 20.50602409638554 75.16 25.71 L75.16 74.29 Q75.1566265060241 79.49397590361446 79.74 77.03 L85.6 73.88 Q89.02010755124772 72.03351217748715 89.02 68.15 Z' fill='%239fa6e0'/%3E%3Crect x='29' y='16' width='42' height='68' rx='6' fill='%232a3fd1'/%3E%3C/svg%3E";

/// `text`, safe inside HTML text and attribute values.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Unix milliseconds `ms` as a UTC date and minute, `2026-11-09 14:03 UTC`.
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "unsigned civil-date arithmetic: every operand is bounded far below u64::MAX, \
              and each subtraction takes away less than it is taken from"
)]
pub fn date(ms: u64) -> String {
    let minutes = ms / 60_000;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let shifted = minutes / (24 * 60) + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minutes / 60 % 24,
        minutes % 60
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <title>{}</title><link rel=\"icon\" type=\"image/svg+xml\" href=\"{ICON}\">\
         <style>{STYLE}</style></head><body><main>{MARK}{body}</main></body></html>",
        escape(title)
    )
}

/// `facts` as one line of separate items, each its own element: the
/// stylesheet spaces them, and a reader or a copy takes each on its own.
fn facts(facts: &[String]) -> String {
    let items = facts.iter().fold(String::new(), |mut items, fact| {
        let _ = write!(items, "<li>{}</li>", escape(fact));
        items
    });
    format!("<ul class=\"facts\">{items}</ul>")
}

/// The person's grants, each with a revoke button; a grant for one workspace
/// by that workspace's name in `names` (by grant, as its host answered now).
#[must_use]
pub fn grants<S: std::hash::BuildHasher>(
    grants: &[Grant],
    names: &std::collections::HashMap<String, String, S>,
    csrf: &str,
) -> String {
    let name_of_grant = |id: &str| {
        grants.iter().find(|grant| grant.id == id).map_or_else(
            || "another key".to_owned(),
            |grant| grant.client_name.clone(),
        )
    };
    let rows = grants.iter().fold(String::new(), |mut rows, grant| {
        let mut reach = vec![match &grant.scope {
            Scope::Account => "All your workspaces".to_owned(),
            // Named by its host when the server could ask it; a host that
            // has not answered yet leaves the plain count.
            Scope::Workspace(_) => names
                .get(&grant.id)
                .cloned()
                .unwrap_or_else(|| "One workspace".to_owned()),
        }];
        match &grant.kind {
            GrantKind::Agent => {}
            GrantKind::Key => reach.push("API key".to_owned()),
            GrantKind::Minted { parent } => {
                reach.push(format!("Made by {}", name_of_grant(parent)));
            }
        }
        if grant.browser_pages {
            reach.push("Operates web pages in Browser".to_owned());
        }
        let mut use_line = vec![
            grant
                .last_used_ms
                .map_or_else(|| "Never used".to_owned(), |ms| format!("Last used {}", date(ms))),
        ];
        if let Some(device) = &grant.created_by {
            use_line.push(format!("Approved on {device}"));
        }
        use_line.push(grant.expires_ms.map_or_else(
            || "Lasts until revoked".to_owned(),
            |ms| format!("Expires {}", date(ms)),
        ));
        let _ = write!(
            rows,
            "<li><span><strong>{}</strong>{}{}</span>\
             <form method=\"post\" action=\"/grants/{}/revoke\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
             <button class=\"danger\">Revoke</button></form></li>",
            escape(&grant.client_name),
            facts(&reach),
            facts(&use_line),
            escape(&grant.id),
            escape(csrf),
        );
        rows
    });
    let list = if grants.is_empty() {
        "<p class=\"quiet\">No agent is connected.</p>".to_owned()
    } else {
        format!("<ul class=\"grants\">{rows}</ul>")
    };
    let new_key = format!(
        "<h2>API keys</h2><p class=\"quiet\">A key lets your own backend or script act in your \
         workspaces: create them, upload files, run actions, and give each of your customers' agents \
         a key for just one workspace. A key lasts until you revoke it, unless you choose when it \
         expires; the keys it gives out expire within 90 days. Revoking a key ends every key it \
         gave out. A key asks you before each command it runs in a workspace's Linux machine, \
         unless you let that workspace's own keys run commands unattended; a key for all your \
         workspaces always asks.</p>\
         <form class=\"new\" method=\"post\" action=\"/grants/keys\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
         <input name=\"name\" required maxlength=\"80\" aria-label=\"What will use it\" placeholder=\"What will use it, like Visa backend\">\
         <select name=\"expires_in\" aria-label=\"When the key expires\">\
         <option value=\"\" selected>Until revoked</option><option value=\"2592000\">30 days</option>\
         <option value=\"7776000\">90 days</option><option value=\"31536000\">1 year</option></select>\
         <label class=\"browser-pages\"><input type=\"checkbox\" name=\"browser_pages\" value=\"on\" checked>\
         Operate web pages in Browser</label>\
         <span class=\"quiet\">The key opens, fills in and clicks the page without asking you each \
         time. Submitting still asks you.</span>\
         <button class=\"primary\">Create API key</button></form>",
        escape(csrf)
    );
    page(
        "Connected agents",
        &format!(
            "<h1>Connected agents</h1><p class=\"quiet\">Revoking ends an agent's sessions at once; it must ask you again to reconnect.</p>{list}{new_key}"
        ),
    )
}

/// The key the person just approved, shown once.
#[must_use]
pub fn key_created(grant: &Grant, token: &str) -> String {
    let pages = if grant.browser_pages {
        " It operates web pages in Browser; submitting a page still asks you."
    } else {
        " It may not operate web pages in Browser."
    };
    let lasts = grant.expires_ms.map_or_else(
        || format!("It works until you revoke it on your grants page.{pages}"),
        |ms| {
            format!(
                "It works until {}, or until you revoke it on your grants page.{pages}",
                date(ms)
            )
        },
    );
    page(
        "Your API key",
        &format!(
            "<h1>Your API key \u{201c}{}\u{201d}</h1>\
             <p>Copy it now: it is not shown again.</p><code class=\"key\">{}</code>\
             <p class=\"quiet\">Send it as <code>Authorization: Bearer &lt;key&gt;</code> to the API at \
             <code>/v1</code>, or set <code>TILO_API_KEY</code> for Tilo's SDKs. \
             {}</p>\
             <div class=\"actions\"><a class=\"button\" href=\"/grants\">Done</a></div>",
            escape(&grant.client_name),
            escape(token),
            escape(&lasts),
        ),
    )
}

/// A short message page: what happened, and what the person can do next
/// where there is something.
#[must_use]
pub fn message(title: &str, text: &str) -> String {
    page(
        title,
        &format!("<h1>{}</h1><p>{}</p>", escape(title), escape(text)),
    )
}

/// A short message page whose one way on is a link.
#[must_use]
pub fn message_action(title: &str, text: &str, action: &str, href: &str) -> String {
    page(
        title,
        &format!(
            "<h1>{}</h1><p>{}</p><div class=\"actions\"><a class=\"button primary\" href=\"{}\">{}</a></div>",
            escape(title),
            escape(text),
            escape(href),
            escape(action)
        ),
    )
}
