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
    View,
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
        let after = rest
            .get(at.saturating_add(mention.len)..)
            .unwrap_or_default();
        // A call written whole, as a refusal ends with the one that works: its arguments
        // follow the tool's name as JSON, and they are part of the call.
        let whole = if mention.arg.is_none() {
            whole_arguments(after)
        } else {
            None
        };
        if let Some((arguments, used)) = whole {
            written.push_str(&render_whole(mention.call, &arguments, surface));
            rest = after.get(used..).unwrap_or_default();
        } else {
            written.push_str(&render(mention.call, mention.arg.as_deref(), surface));
            rest = after;
        }
    }
    written.push_str(rest);
    written
}

/// The arguments of a call written whole (`{"path":"notes"}` after the tool's name and a
/// blank), and how many bytes of `after` the blank and they cover. None where no JSON object
/// stands there: the name is then a word in a sentence.
fn whole_arguments(after: &str) -> Option<(serde_json::Map<String, serde_json::Value>, usize)> {
    let written = after.strip_prefix(' ')?;
    if !written.starts_with('{') {
        return None;
    }
    let mut read = serde_json::Deserializer::from_str(written).into_iter::<serde_json::Value>();
    let Some(Ok(serde_json::Value::Object(arguments))) = read.next() else {
        return None;
    };
    Some((arguments, read.byte_offset().saturating_add(1)))
}

/// A call written whole, as `surface` writes it: with the path, the search or the invocation
/// its arguments name, and for an action what it is given, each blank kept.
fn render_whole(
    call: Call,
    arguments: &serde_json::Map<String, serde_json::Value>,
    surface: Surface,
) -> String {
    let named = |name: &str| {
        arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
    };
    let arg = match call {
        Call::Status => named("invocation_id"),
        Call::Find => named("query"),
        _ => named("path"),
    };
    let given = arguments
        .get("args")
        .and_then(serde_json::Value::as_object)
        .filter(|given| !given.is_empty());
    match (call, surface, arg, given) {
        // The route of one invocation, where the call names it.
        (Call::Status, Surface::Rest, Some(id), _) => {
            format!("GET /v1/workspaces/{WORKSPACE}/invocations/{id}")
        }
        (Call::Do, Surface::Cli, Some(_), Some(given)) => {
            let mut line = render(call, arg, surface);
            for (name, value) in given {
                line.push(' ');
                line.push_str(&as_the_command_reads(name, value));
            }
            line
        }
        (Call::Do, Surface::Rest, Some(_), Some(given)) => format!(
            "{} {}",
            render(call, arg, surface),
            serde_json::Value::Object(given.clone())
        ),
        _ => render(call, arg, surface),
    }
}

/// One argument as the command reads it, `name=value`: a text as it is, anything else as
/// JSON, in quotes where a shell would part it.
fn as_the_command_reads(name: &str, value: &serde_json::Value) -> String {
    let written = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let parted = written
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '{' | '['));
    if parted {
        format!("{name}='{}'", written.replace('\'', "'\\''"))
    } else {
        format!("{name}={written}")
    }
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
    ("tilo_view", &Call::View),
    ("tilo_read", &Call::Read),
    ("tilo_do", &Call::Do),
    ("tilo_ls", &Call::Ls),
];

/// The command's verbs, longest first.
const CLI: &[(&str, &Call)] = &[
    ("tilo read", &Call::Read),
    ("tilo view", &Call::View),
    ("tilo do", &Call::Do),
    ("tilo ls", &Call::Ls),
];

/// The workspace a route is written under, as the docs write it.
const WORKSPACE: &str = "…";

/// A route of this API, as [`render`] writes one, and the call it is.
fn rest_mention(text: &str) -> Option<Mention> {
    let (method, rest) = if let Some(rest) = text.strip_prefix("GET ") {
        ("GET", rest)
    } else {
        ("POST", text.strip_prefix("POST ")?)
    };
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
        if let Some(arg) = rest.strip_prefix("/views?path=") {
            let (arg, used) = token(arg);
            return Some(Mention {
                call: Call::View,
                arg: (!arg.is_empty()).then(|| arg.to_owned()),
                len: text.len().saturating_sub(used.len()),
            });
        }
        if let Some(arg) = rest.strip_prefix("/invocations/") {
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
    // A placeholder names no path: a person types the verb alone, rather than
    // a template nobody filled in.
    let named = |prefix: &str| match arg.filter(|arg| !arg.contains('{')) {
        Some(arg) => format!("{prefix} {arg}"),
        None => prefix.to_owned(),
    };
    match surface {
        Surface::Mcp => named(match call {
            Call::Ls => "tilo_ls",
            Call::Read => "tilo_read",
            Call::View => "tilo_view",
            Call::Do => "tilo_do",
            Call::Find => "tilo_find",
            Call::Link => "tilo_link",
            Call::Status => "tilo_status",
        }),
        Surface::Cli => match call {
            Call::Ls => named("tilo ls"),
            Call::Read => named("tilo read"),
            Call::View => named("tilo view"),
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
            Call::View => arg.map_or_else(
                || format!("GET /v1/workspaces/{WORKSPACE}/views"),
                |arg| format!("GET /v1/workspaces/{WORKSPACE}/views?path={arg}"),
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
            Call::Find => {
                format!("POST /v1/workspaces/{WORKSPACE}/resources/search/actions/find")
            }
            Call::Link => format!(
                "POST /v1/workspaces/{WORKSPACE}/resources/{}/actions/share-link",
                arg.unwrap_or("{path}")
            ),
            Call::Status => format!("GET /v1/workspaces/{WORKSPACE}/invocations/{{invocation}}"),
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
        // A route with a placeholder names no path, so the command says its
        // verb alone, never a template nobody filled in.
        assert_eq!(
            said(
                "start it with POST /v1/workspaces/…/resources/{path}/actions/{action}",
                Surface::Cli
            ),
            "start it with tilo do"
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

    /// A refusal ends with the call that works, written whole as an agent's tool takes it.
    /// A person at the command reads it as the line they type, a caller of the API as the
    /// request it sends: the same path, the same arguments, the same blanks.
    #[test]
    fn a_call_written_whole_is_the_commands_line_and_the_apis_request() {
        let refused = "required argument `title` was not supplied\nThe call that works, with \
            your own value where one stands in angle brackets: tilo_do \
            {\"path\":\"notes/Todo#rename\",\"args\":{\"title\":\"<title>\"}}";
        assert_eq!(
            said(refused, Surface::Cli),
            "required argument `title` was not supplied\nThe call that works, with your own \
             value where one stands in angle brackets: tilo do notes/Todo#rename title=<title>"
        );
        assert_eq!(
            said(refused, Surface::Rest),
            "required argument `title` was not supplied\nThe call that works, with your own \
             value where one stands in angle brackets: POST \
             /v1/workspaces/…/resources/notes/Todo/actions/rename {\"title\":\"<title>\"}"
        );
        // What the caller gave is kept, each value as the command reads one.
        let given = "tilo_do {\"path\":\"so.tilo.tasks#new_task\",\"args\":{\"count\":3,\
            \"detail\":{\"kind\":\"login\"},\"title\":\"Buy plums\"}}";
        assert_eq!(
            said(given, Surface::Cli),
            "tilo do so.tilo.tasks#new_task count=3 detail='{\"kind\":\"login\"}' title='Buy plums'"
        );
        // A listing and an invocation, each with what it names.
        let listings = "Calls that work:\n  tilo_ls {\"path\":\"notes/Todo\"}\n  tilo_ls {}";
        assert_eq!(
            said(listings, Surface::Cli),
            "Calls that work:\n  tilo ls notes/Todo\n  tilo ls"
        );
        assert_eq!(
            said(listings, Surface::Rest),
            "Calls that work:\n  GET /v1/workspaces/…/resources?path=notes/Todo\n  GET \
             /v1/workspaces/…/resources"
        );
        assert_eq!(
            said("tilo_status {\"invocation_id\":\"6a10\"}", Surface::Rest),
            "GET /v1/workspaces/…/invocations/6a10"
        );
        // An agent's own surface reads it as it was written.
        assert_eq!(said(refused, Surface::Mcp), refused);
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
            said(
                "go to GET /v1/workspaces/…/invocations/run-1",
                Surface::Rest
            ),
            "go to GET /v1/workspaces/…/invocations/{invocation}"
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
