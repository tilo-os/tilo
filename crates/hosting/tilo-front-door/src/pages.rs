//! The pages a person sees at the door: the grants they gave, each
//! revocable, and short messages. Approving an agent happens in the
//! person's own Tilo, which holds what signs for them.
//!
//! Plain server-rendered HTML with no script, so a decision is the form the
//! person submits and nothing else on the page can make it.

use crate::directory::Workspace;
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
form.new{display:flex;gap:.5rem;flex-wrap:wrap;margin-top:1.25rem}form.new input{flex:1;min-width:10rem;font:inherit;padding:.55rem .7rem;border:1px solid var(--line);border-radius:9px;background:transparent;color:var(--text)}h2{font-size:1.05rem;margin:1.5rem 0 0}ul.grants{list-style:none;padding:0;margin:1rem 0 0}ul.grants li{display:flex;justify-content:space-between;align-items:center;gap:1rem;padding:.75rem 0;border-top:1px solid var(--line)}
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
pub fn grants(grants: &[Grant], workspaces: &[Workspace], csrf: &str) -> String {
    let name_of = |id: &str| {
        workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .map_or_else(|| id.to_owned(), |workspace| workspace.name.clone())
    };
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
                Scope::Workspace(id) => format!("Only {}", name_of(id)),
            };
            match &grant.kind {
                GrantKind::Agent => {}
                GrantKind::Key => reach.push_str(" \u{b7} API key"),
                GrantKind::Minted { parent } => {
                    let _ = write!(reach, " \u{b7} made by {}", name_of_grant(parent));
                }
            }
            let _ = write!(
                rows,
                "<li><span><strong>{}</strong><br><span class=\"muted\">{}</span></span>\
                 <form method=\"post\" action=\"/grants/{}/revoke\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
                 <button class=\"danger\">Revoke</button></form></li>",
                escape(&grant.client_name),
                escape(&reach),
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
         workspace. Revoking a key ends every key it gave out.</p>\
         <form class=\"new\" method=\"post\" action=\"/grants/keys\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
         <input name=\"name\" required maxlength=\"80\" placeholder=\"What will use it, like Visa backend\">\
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
    page(
        "Your API key",
        &format!(
            "<h1>Your API key \u{201c}{}\u{201d}</h1>\
             <p>Copy it now: it is not shown again.</p><code class=\"key\">{}</code>\
             <p class=\"muted\">Send it as <code>Authorization: Bearer &lt;key&gt;</code> to the API at \
             <code>/v1</code>, or set <code>TILO_API_KEY</code> for the <code>tilo</code> command. \
             It works until you revoke it on your grants page.</p>\
             <div class=\"actions\"><a class=\"button\" href=\"/grants\">Done</a></div>",
            escape(&grant.client_name),
            escape(token),
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
