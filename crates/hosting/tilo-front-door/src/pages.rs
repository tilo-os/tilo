//! The pages a person sees at the door: the grants they gave, each
//! revocable, and short messages. Approving an agent happens in the
//! person's own Tilo, which holds what signs for them.
//!
//! Plain server-rendered HTML with no script, so a decision is the form the
//! person submits and nothing else on the page can make it.

use crate::store::{Grant, GrantKind, Scope};
use std::fmt::Write as _;

const STYLE: &str = "
:root{color-scheme:light dark;--bg:#f6f5f2;--card:#fff;--text:#1c1b19;--muted:#6b6862;--line:#e3e0da;--accent:#2f5bd3;--accent-text:#fff;--danger:#b3261e}
@media (prefers-color-scheme:dark){:root{--bg:#141413;--card:#1e1e1c;--text:#ecebe8;--muted:#a19e97;--line:#33322f;--accent:#7c9cf5;--accent-text:#0d0d0c;--danger:#f2b8b5}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.5 system-ui,-apple-system,'Segoe UI',sans-serif}
main{max-width:30rem;margin:0 auto;padding:12vh 16px 4rem}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:1.75rem}
h1{font-size:1.35rem;line-height:1.3;margin:0 0 .5rem}p{margin:.5rem 0}.muted{color:var(--muted);font-size:.92rem}
.actions{display:flex;gap:.6rem;justify-content:flex-end;margin-top:1.25rem;flex-wrap:wrap}
button,a.button{font:inherit;border-radius:9px;padding:.6rem 1.1rem;border:1px solid var(--line);background:transparent;color:var(--text);cursor:pointer;text-decoration:none;display:inline-block}
button.primary,a.button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-text)}
button.danger{color:var(--danger)}code.key{display:block;word-break:break-all;padding:.75rem;border:1px solid var(--line);border-radius:9px;margin:.75rem 0;font-size:.9rem}
form.new{display:flex;gap:.5rem;flex-wrap:wrap;margin-top:1.25rem}form.new input{flex:1;min-width:10rem}
form.new label.browser-pages{display:flex;align-items:center;gap:.5rem;flex-basis:100%;font-size:.95rem}
form.new label.browser-pages input{flex:none;min-width:0;margin:0}form.new span.muted{flex-basis:100%;margin-top:-.25rem}form.new input,form.new select{font:inherit;padding:.55rem .7rem;border:1px solid var(--line);border-radius:9px;background:var(--card);color:var(--text)}h2{font-size:1.05rem;margin:1.5rem 0 0}ul.grants{list-style:none;padding:0;margin:1rem 0 0}ul.grants li{display:flex;justify-content:space-between;align-items:center;gap:1rem;padding:.75rem 0;border-top:1px solid var(--line)}
";

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
         <title>{}</title><style>{STYLE}</style></head><body><main><div class=\"card\">{body}</div></main></body></html>",
        escape(title)
    )
}

/// The person's grants, each with a revoke button.
#[must_use]
pub fn grants(grants: &[Grant], csrf: &str) -> String {
    let name_of_grant = |id: &str| {
        grants
            .iter()
            .find(|grant| grant.id == id)
            .map_or_else(|| id.to_owned(), |grant| grant.client_name.clone())
    };
    let rows = grants
        .iter()
        .fold(String::new(), |mut rows, grant| {
            let mut reach = match &grant.scope {
                Scope::Account => "All your workspaces".to_owned(),
                Scope::Workspace(id) => format!("Only workspace {id}"),
            };
            match &grant.kind {
                GrantKind::Agent => {}
                GrantKind::Key => reach.push_str(" \u{b7} API key"),
                GrantKind::Minted { parent } => {
                    let _ = write!(reach, " \u{b7} made by {}", name_of_grant(parent));
                }
            }
            if grant.browser_pages {
                reach.push_str(" \u{b7} operates web pages in Browser");
            }
            let mut use_line = grant
                .last_used_ms
                .map_or_else(|| "Never used".to_owned(), |ms| format!("Last used {}", date(ms)));
            if let Some(device) = &grant.created_by {
                let _ = write!(use_line, " \u{b7} approved on {device}");
            }
            match grant.expires_ms {
                Some(ms) => {
                    let _ = write!(use_line, " \u{b7} expires {}", date(ms));
                }
                None => use_line.push_str(" \u{b7} lasts until revoked"),
            }
            let _ = write!(
                rows,
                "<li><span><strong>{}</strong><br><span class=\"muted\">{}</span>\
                 <br><span class=\"muted\">{}</span></span>\
                 <form method=\"post\" action=\"/grants/{}/revoke\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
                 <button class=\"danger\">Revoke</button></form></li>",
                escape(&grant.client_name),
                escape(&reach),
                escape(&use_line),
                escape(&grant.id),
                escape(csrf),
            );
            rows
        });
    let list = if grants.is_empty() {
        "<p class=\"muted\">No agent is connected.</p>".to_owned()
    } else {
        format!("<ul class=\"grants\">{rows}</ul>")
    };
    let new_key = format!(
        "<h2>API keys</h2><p class=\"muted\">A key lets your own backend or script act in your workspaces: \
         create them, upload files, run actions, and give each of your customers' agents a key for just one \
         workspace. A key lasts until you revoke it, unless you choose when it expires; the keys it \
         gives out expire within 90 days. Revoking a key ends every key it gave out.</p>\
         <form class=\"new\" method=\"post\" action=\"/grants/keys\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
         <input name=\"name\" required maxlength=\"80\" placeholder=\"What will use it, like Visa backend\">\
         <select name=\"expires_in\" aria-label=\"When the key expires\">\
         <option value=\"\" selected>Until revoked</option><option value=\"2592000\">30 days</option>\
         <option value=\"7776000\">90 days</option><option value=\"31536000\">1 year</option></select>\
         <label class=\"browser-pages\"><input type=\"checkbox\" name=\"browser_pages\" value=\"on\" checked>\
         Operate web pages in Browser</label>\
         <span class=\"muted\">The key opens, fills in and clicks the page without asking you each\
         time. Submitting still asks you.</span>\
         <button class=\"primary\">Create API key</button></form>",
        escape(csrf)
    );
    page(
        "Connected agents",
        &format!(
            "<h1>Connected agents</h1><p class=\"muted\">Revoking ends an agent's sessions at once; it must ask you again to reconnect.</p>{list}{new_key}"
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
             <p class=\"muted\">Send it as <code>Authorization: Bearer &lt;key&gt;</code> to the API at \
             <code>/v1</code>, or set <code>TILO_API_KEY</code> for the <code>tilo</code> command. \
             {}</p>\
             <div class=\"actions\"><a class=\"button\" href=\"/grants\">Done</a></div>",
            escape(&grant.client_name),
            escape(token),
            escape(&lasts),
        ),
    )
}

/// A short message page.
#[must_use]
pub fn message(title: &str, text: &str) -> String {
    page(
        title,
        &format!("<h1>{}</h1><p>{}</p>", escape(title), escape(text)),
    )
}
