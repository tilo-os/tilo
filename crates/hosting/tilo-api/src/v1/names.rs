//! What an answer calls the calls it names, on the surface that reads it.
//!
//! A refusal names what would work instead: an agent's MCP tools
//! (`tilo_ls notes`), a route of this API
//! (`GET /v1/workspaces/…/resources?path=notes`), or a verb of the `tilo`
//! command (`tilo ls notes`). Every error a tool wrote names MCP tools; the
//! API and the command each rewrite those, and anything already written in
//! their own vocabulary, into the words their reader can act on.

/// Who reads an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// An MCP client's agent: the tools themselves.
    Mcp,
    /// A caller of this API: its routes.
    Rest,
    /// A person at the `tilo` command: its verbs.
    Cli,
}

/// The calls an answer may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Ls,
    Read,
    Do,
    Find,
    Link,
    Status,
}

/// One mention of a call in `text`, as it was written there.
struct Mention {
    call: Call,
    /// The path, query or run it was given, when it named one.
    arg: Option<String>,
    /// How many bytes of the text the mention covers.
    len: usize,
}

/// `text` with each call it names written as `surface`'s reader knows it.
#[must_use]
pub fn said(text: &str, surface: Surface) -> String {
    if surface == Surface::Mcp {
        return text.to_owned();
    }
    let mut written = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, mention)) = next_mention(rest) {
        written.push_str(rest.get(..at).unwrap_or_default());
        written.push_str(&render(mention.call, mention.arg.as_deref(), surface));
        rest = rest
            .get(at.saturating_add(mention.len)..)
            .unwrap_or_default();
    }
    written.push_str(rest);
    written
}

/// The first call `text` names, and where.
fn next_mention(text: &str) -> Option<(usize, Mention)> {
    (0..text.len())
        .filter(|at| text.is_char_boundary(*at))
        // A word carries no tool inside it: `xtilo_ls` is a word of its own.
        .filter(|at| {
            *at == 0
                || text
                    .get(..*at)
                    .and_then(|before| before.chars().next_back())
                    .is_none_or(|before| !(before.is_alphanumeric() || before == '_'))
        })
        .find_map(|at| mention_at(text.get(at..)?).map(|mention| (at, mention)))
}

/// The call `text` starts with, when it starts with one.
fn mention_at(text: &str) -> Option<Mention> {
    for (spelling, call) in MCP {
        if let Some(mention) = named_call(text, spelling, **call) {
            return Some(mention);
        }
    }
    for (spelling, call) in CLI {
        if let Some(mention) = named_call(text, spelling, **call) {
            return Some(mention);
        }
    }
    rest_mention(text)
}

/// The mention of `call`, written `spelling` at the start of `text`, with the
/// path after it when it names one: the mention covers both.
fn named_call(text: &str, spelling: &str, call: Call) -> Option<Mention> {
    let after = word_after(text, spelling)?;
    let named = text.len().saturating_sub(after.len());
    let arg = argument(after);
    let len = match &arg {
        Some(arg) => named.saturating_add(1).saturating_add(arg.len()),
        None => named,
    };
    Some(Mention { call, arg, len })
}

/// The text after `spelling` when `text` starts with it as a whole word.
fn word_after<'a>(text: &'a str, spelling: &str) -> Option<&'a str> {
    let after = text.strip_prefix(spelling)?;
    after
        .chars()
        .next()
        .is_none_or(|next| !(next.is_alphanumeric() || next == '_'))
        .then_some(after)
}

/// The MCP tools' names, longest first so a prefix never wins.
const MCP: &[(&str, &Call)] = &[
    ("tilo_status", &Call::Status),
    ("tilo_find", &Call::Find),
    ("tilo_link", &Call::Link),
    ("tilo_read", &Call::Read),
    ("tilo_do", &Call::Do),
    ("tilo_ls", &Call::Ls),
];

/// The command's verbs, longest first.
const CLI: &[(&str, &Call)] = &[
    ("tilo read", &Call::Read),
    ("tilo do", &Call::Do),
    ("tilo ls", &Call::Ls),
];

/// The workspace a route is written under, as the docs write it.
const WORKSPACE: &str = "…";

/// A route of this API, as [`render`] writes one, and the call it is.
fn rest_mention(text: &str) -> Option<Mention> {
    let (method, rest) = ["GET", "POST"].into_iter().find_map(|method| {
        text.strip_prefix(method)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(|rest| (method, rest))
    })?;
    let rest = rest.strip_prefix("/v1/workspaces/")?;
    let rest = rest
        .strip_prefix(WORKSPACE)
        .or_else(|| rest.strip_prefix("{workspace}"))?;
    if method == "GET" {
        if let Some(arg) = rest.strip_prefix("/resources?path=") {
            let (arg, used) = token(arg);
            let path = (!arg.is_empty()).then(|| arg.to_owned());
            return Some(Mention {
                call: Call::Ls,
                arg: path,
                len: text.len().saturating_sub(used.len()),
            });
        }
        if let Some(arg) = rest.strip_prefix("/search?q=") {
            let (arg, used) = token(arg);
            return Some(Mention {
                call: Call::Find,
                arg: (!arg.is_empty()).then(|| arg.to_owned()),
                len: text.len().saturating_sub(used.len()),
            });
        }
        if let Some(arg) = rest.strip_prefix("/links?path=") {
            let (arg, used) = token(arg);
            return Some(Mention {
                call: Call::Link,
                arg: (!arg.is_empty()).then(|| arg.to_owned()),
                len: text.len().saturating_sub(used.len()),
            });
        }
        if let Some(arg) = rest.strip_prefix("/runs/") {
            let (arg, used) = token(arg);
            return Some(Mention {
                call: Call::Status,
                arg: (!arg.is_empty()).then(|| arg.to_owned()),
                len: text.len().saturating_sub(used.len()),
            });
        }
        if let Some(arg) = rest.strip_prefix("/resources/") {
            let (arg, used) = token(arg);
            return Some(Mention {
                call: Call::Read,
                arg: (!arg.is_empty()).then(|| arg.to_owned()),
                len: text.len().saturating_sub(used.len()),
            });
        }
        rest.strip_prefix("/resources")?;
        return Some(Mention {
            call: Call::Ls,
            arg: None,
            len: text.len().saturating_sub(rest.len()),
        });
    }
    let rest = rest.strip_prefix("/resources/")?;
    let (whole, remaining) = token(rest);
    // A resource's own path may hold segments of its own; the action is the
    // last `/actions/<action>`.
    let (path, action) = whole
        .rsplit_once("/actions/")
        .map_or((whole, None), |(path, action)| (path, Some(action)));
    let arg = match action {
        Some(action) => Some(format!("{path}#{action}")),
        None => (!path.is_empty()).then(|| path.to_owned()),
    };
    Some(Mention {
        call: Call::Do,
        arg,
        len: text.len().saturating_sub(remaining.len()),
    })
}

/// The first token of `text`, which ends at a space or a sentence's
/// punctuation, and the rest after it.
fn token(text: &str) -> (&str, &str) {
    let end = text
        .find(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | '.' | ')' | ']'))
        .unwrap_or(text.len());
    text.split_at(end)
}

/// A mention's argument: the path that follows it, when the word after is a
/// path and not the sentence going on.
fn argument(after: &str) -> Option<String> {
    let after = after.strip_prefix(' ')?;
    let (token, _) = token(after);
    // A path, not the start of a call's arguments (`{path: "notes#open"}`).
    let path = token
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ':' | '#' | '@'));
    (path && !token.is_empty() && !STOP.contains(&token)).then(|| token.to_owned())
}

/// Words that follow a tool's name in a sentence and name no path.
const STOP: &[&str] = &[
    "lists", "list", "reads", "read", "names", "shows", "finds", "issued", "again", "is", "for",
    "and", "or", "to", "the", "a", "its", "what", "with", "in", "on", "of",
];

/// One call, as `surface` writes it.
fn render(call: Call, arg: Option<&str>, surface: Surface) -> String {
    let named = |prefix: &str| match arg {
        Some(arg) => format!("{prefix} {arg}"),
        None => prefix.to_owned(),
    };
    match surface {
        Surface::Mcp => named(match call {
            Call::Ls => "tilo_ls",
            Call::Read => "tilo_read",
            Call::Do => "tilo_do",
            Call::Find => "tilo_find",
            Call::Link => "tilo_link",
            Call::Status => "tilo_status",
        }),
        Surface::Cli => match call {
            Call::Ls => named("tilo ls"),
            Call::Read => named("tilo read"),
            Call::Do => named("tilo do"),
            // The command has no verb of its own for these: it says what
            // they are instead of naming a tool its reader does not have.
            Call::Find => "a search".to_owned(),
            Call::Link => "a link".to_owned(),
            Call::Status => "a run".to_owned(),
        },
        Surface::Rest => match call {
            Call::Ls => arg.map_or_else(
                || format!("GET /v1/workspaces/{WORKSPACE}/resources"),
                |arg| format!("GET /v1/workspaces/{WORKSPACE}/resources?path={arg}"),
            ),
            Call::Read => arg.map_or_else(
                || format!("GET /v1/workspaces/{WORKSPACE}/resources"),
                |arg| format!("GET /v1/workspaces/{WORKSPACE}/resources/{arg}"),
            ),
            Call::Do => arg.map_or_else(
                || format!("POST /v1/workspaces/{WORKSPACE}/resources/{{path}}/actions/{{action}}"),
                |arg| match arg.split_once('#') {
                    Some((path, action)) => {
                        format!("POST /v1/workspaces/{WORKSPACE}/resources/{path}/actions/{action}")
                    }
                    None => format!(
                        "POST /v1/workspaces/{WORKSPACE}/resources/{arg}/actions/{{action}}"
                    ),
                },
            ),
            Call::Find => arg.map_or_else(
                || format!("GET /v1/workspaces/{WORKSPACE}/search"),
                |arg| format!("GET /v1/workspaces/{WORKSPACE}/search?q={arg}"),
            ),
            Call::Link => arg.map_or_else(
                || format!("GET /v1/workspaces/{WORKSPACE}/links"),
                |arg| format!("GET /v1/workspaces/{WORKSPACE}/links?path={arg}"),
            ),
            Call::Status => format!("GET /v1/workspaces/{WORKSPACE}/runs/{{run}}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_mcp_tool_is_a_route_for_the_api_and_a_verb_for_the_command() {
        assert_eq!(
            said(
                "name the action: notes#open; tilo_ls notes lists them",
                Surface::Rest
            ),
            "name the action: notes#open; GET /v1/workspaces/…/resources?path=notes lists them"
        );
        assert_eq!(
            said(
                "name the action: notes#open; tilo_ls notes lists them",
                Surface::Cli
            ),
            "name the action: notes#open; tilo ls notes lists them"
        );
        assert_eq!(
            said("no such note; tilo_read notes/Todo reads it", Surface::Cli),
            "no such note; tilo read notes/Todo reads it"
        );
        assert_eq!(
            said("it is not open; tilo_ls lists what is inside", Surface::Cli),
            "it is not open; tilo ls lists what is inside"
        );
        assert_eq!(
            said("run tilo_do notes/Todo#archive", Surface::Rest),
            "run POST /v1/workspaces/…/resources/notes/Todo/actions/archive"
        );
        assert_eq!(
            said("run tilo_do {path: \"notes#open\"}", Surface::Rest),
            "run POST /v1/workspaces/…/resources/{path}/actions/{action} {path: \"notes#open\"}"
        );
        // A route already written is the command's verb, argument and all.
        assert_eq!(
            said(
                "POST /v1/workspaces/…/resources/notes/Todo/actions/archive is the call",
                Surface::Cli
            ),
            "tilo do notes/Todo#archive is the call"
        );
    }

    #[test]
    fn a_route_is_the_same_call_on_the_other_surfaces() {
        let route = "GET /v1/workspaces/…/resources?path=tasks lists its actions";
        assert_eq!(said(route, Surface::Cli), "tilo ls tasks lists its actions");
        assert_eq!(
            said(
                "GET /v1/workspaces/{workspace}/resources/notes/Todo?cursor=x reads it",
                Surface::Mcp
            ),
            "GET /v1/workspaces/{workspace}/resources/notes/Todo?cursor=x reads it"
        );
        assert_eq!(
            said("go to GET /v1/workspaces/…/runs/run-1", Surface::Rest),
            "go to GET /v1/workspaces/…/runs/{run}"
        );
    }

    #[test]
    fn words_that_are_no_call_are_left_alone() {
        for text in [
            "the listing names it",
            "tilo_ls2 is no tool",
            "xtilo_ls is a word of its own",
            "nothing to rewrite here",
        ] {
            assert_eq!(said(text, Surface::Rest), text, "{text}");
        }
    }
}
