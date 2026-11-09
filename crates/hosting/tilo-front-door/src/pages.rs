//! The pages a person sees: signing in, choosing what an agent may reach,
//! and the grants they gave, each revocable.
//!
//! Plain server-rendered HTML with no script, so the decision is the form
//! the person submits and nothing else on the page can make it.

use crate::directory::Workspace;
use crate::store::{Grant, Scope};
use std::fmt::Write as _;

const STYLE: &str = "
:root{color-scheme:light dark;--bg:#f6f5f2;--card:#fff;--text:#1c1b19;--muted:#6b6862;--line:#e3e0da;--accent:#2f5bd3;--accent-text:#fff;--danger:#b3261e}
@media (prefers-color-scheme:dark){:root{--bg:#141413;--card:#1e1e1c;--text:#ecebe8;--muted:#a19e97;--line:#33322f;--accent:#7c9cf5;--accent-text:#0d0d0c;--danger:#f2b8b5}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.5 system-ui,-apple-system,'Segoe UI',sans-serif}
main{max-width:30rem;margin:0 auto;padding:12vh 16px 4rem}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:1.75rem}
h1{font-size:1.35rem;line-height:1.3;margin:0 0 .5rem}p{margin:.5rem 0}.muted{color:var(--muted);font-size:.92rem}
fieldset{border:0;padding:0;margin:1.25rem 0}label.choice{display:flex;gap:.75rem;align-items:flex-start;padding:.85rem;border:1px solid var(--line);border-radius:10px;margin-bottom:.6rem;cursor:pointer}
label.choice:has(input:checked){border-color:var(--accent)}label.choice input{margin-top:.3rem}
select{width:100%;margin-top:.5rem;padding:.5rem;border-radius:8px;border:1px solid var(--line);background:var(--bg);color:var(--text);font:inherit}
.actions{display:flex;gap:.6rem;justify-content:flex-end;margin-top:1.25rem;flex-wrap:wrap}
button,a.button{font:inherit;border-radius:9px;padding:.6rem 1.1rem;border:1px solid var(--line);background:transparent;color:var(--text);cursor:pointer;text-decoration:none;display:inline-block}
button.primary,a.button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-text)}
button.danger{color:var(--danger)}ul.grants{list-style:none;padding:0;margin:1rem 0 0}ul.grants li{display:flex;justify-content:space-between;align-items:center;gap:1rem;padding:.75rem 0;border-top:1px solid var(--line)}
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

/// Ask the person to sign in with their Tilo.
#[must_use]
pub fn sign_in(client: &str, sign_in_url: &str) -> String {
    page(
        "Sign in to Tilo",
        &format!(
            "<h1>Sign in to connect {client}</h1>\
             <p class=\"muted\">{client} asks to work in your Tilo. Sign in with your Tilo to choose what it may reach.</p>\
             <div class=\"actions\"><a class=\"button primary\" href=\"{url}\">Sign in with Tilo</a></div>",
            client = escape(client),
            url = escape(sign_in_url),
        ),
    )
}

/// The scope choice: every workspace, or one. `pinned` when the agent was
/// pointed at one workspace, which is then the only choice.
#[must_use]
pub fn consent(
    client: &str,
    action: &str,
    csrf: &str,
    workspaces: &[Workspace],
    pinned: Option<&Workspace>,
) -> String {
    let client = escape(client);
    let hidden = format!(
        "<input type=\"hidden\" name=\"csrf\" value=\"{}\">",
        escape(csrf)
    );
    let choice = if let Some(workspace) = pinned {
        format!(
            "<p>It will work only in <strong>{}</strong>, and cannot see or reach your other workspaces.</p>\
             <input type=\"hidden\" name=\"scope\" value=\"workspace\">\
             <input type=\"hidden\" name=\"workspace\" value=\"{}\">",
            escape(&workspace.name),
            escape(&workspace.id)
        )
    } else {
        let options = workspaces
            .iter()
            .fold(String::new(), |mut options, workspace| {
                let _ = write!(
                    options,
                    "<option value=\"{}\">{}</option>",
                    escape(&workspace.id),
                    escape(&workspace.name)
                );
                options
            });
        let one = if workspaces.is_empty() {
            "<label class=\"choice\"><input type=\"radio\" name=\"scope\" value=\"workspace\" disabled>\
             <span><strong>One workspace</strong><br><span class=\"muted\">You have no workspaces yet.</span></span></label>"
                .to_owned()
        } else {
            format!(
                "<label class=\"choice\"><input type=\"radio\" name=\"scope\" value=\"workspace\">\
                 <span style=\"flex:1\"><strong>One workspace</strong><br>\
                 <span class=\"muted\">Only this workspace. It cannot see or reach the others.</span>\
                 <select name=\"workspace\" aria-label=\"Workspace\">{options}</select></span></label>"
            )
        };
        format!(
            "<fieldset><legend class=\"muted\">What may it reach?</legend>\
             <label class=\"choice\"><input type=\"radio\" name=\"scope\" value=\"account\" checked>\
             <span><strong>All my workspaces</strong><br>\
             <span class=\"muted\">It can list your workspaces and create new ones, and names the workspace each time it acts.</span></span></label>\
             {one}</fieldset>"
        )
    };
    page(
        "Connect to Tilo",
        &format!(
            "<h1>Let {client} work in your Tilo?</h1>\
             <p class=\"muted\">It acts as an agent of yours: what it does is shown and attributed to it, and you can stop it or revoke this any time.</p>\
             <form method=\"post\" action=\"{action}\">{hidden}{choice}\
             <div class=\"actions\"><button name=\"decision\" value=\"deny\">Cancel</button>\
             <button class=\"primary\" name=\"decision\" value=\"allow\">Allow</button></div></form>",
            action = escape(action),
        ),
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
    let rows = grants
        .iter()
        .fold(String::new(), |mut rows, grant| {
            let reach = match &grant.scope {
                Scope::Account => "All your workspaces".to_owned(),
                Scope::Workspace(id) => format!("Only {}", name_of(id)),
            };
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
    page(
        "Connected agents",
        &format!(
            "<h1>Connected agents</h1><p class=\"muted\">Revoking ends an agent's sessions at once; it must ask you again to reconnect.</p>{list}"
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
