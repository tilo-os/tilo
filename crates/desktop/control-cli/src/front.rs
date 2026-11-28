//! A desktop's own actions from a command: open an app (`tilo open`, `app open`), run an
//! action at the address a verb spells itself, and say what an action answered, as `tilo do`
//! says it too. Each sends the action model's own frames
//! ([`RemoteActionFrame`]) through the first socket that serves this process
//! ([`crate::client`]): on a desktop, the device admin's socket at their own
//! login, or the session's MCP server from inside an app such as Terminal,
//! which acts with that app's workspace scope; where no desktop session runs,
//! the device's daemon. Nothing here picks authority or a workspace; the
//! device decides both.

use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
    application_open_target, provider_workspace,
};
use tilo_actions::tree::app_meant;
use tilo_actions::{ActionDescriptor, InvocationId, InvocationUpdate};

use crate::client::{ControlSocket, Desktop, Unreachable};

/// How long one wait for an invocation lasts: the desktop holds the answer
/// until the action settles or this runs out, and it is asked again.
const RUN_WAIT_MS: u32 = 5_000;

/// Pages one listing reads at most.
const LIST_PAGES: usize = 8;

/// The installed-app address that opens files and links in an app.
const OPEN_WITH_PREFIX: &str = "open-with:";

/// How this program's listing command is spelled where a refusal points to it:
/// `tilo action list`, or the standalone `action list`.
#[derive(Clone, Debug)]
pub struct Spelling {
    pub list: String,
}

impl Spelling {
    /// `tilo action list`.
    #[must_use]
    pub fn tilo() -> Self {
        Self {
            list: "tilo action list".into(),
        }
    }

    /// The standalone `action list`.
    #[must_use]
    pub fn standalone() -> Self {
        Self {
            list: "action list".into(),
        }
    }
}

/// Where a command writes, so tests can read it.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// The one sentence a person reads when the desktop could not be reached.
#[must_use]
pub fn unreachable_sentence(why: &Unreachable) -> String {
    match why {
        Unreachable::NoDesktop => {
            "Tilo isn't answering on this computer: the session that holds your workspaces is \
             not running. Run `tilo status` to see how it stands."
                .into()
        }
        Unreachable::Refused {
            socket: ControlSocket::Agent(_),
            ..
        } => "The desktop didn't let this terminal use its actions.".into(),
        Unreachable::Refused { .. } => {
            "The desktop didn't let this program use its actions.".into()
        }
        Unreachable::Io { .. } => "Tilo stopped answering. Try again.".into(),
    }
}

/// The sentence a person reads for a refusal of the desktop.
fn refused(error: &ActionCallError, spelling: &Spelling) -> String {
    match error {
        ActionCallError::UnknownAction => format!(
            "That isn't something this desktop can do now. See what it can: {}",
            spelling.list
        ),
        ActionCallError::NotPermitted => "You haven't allowed that here.".into(),
        other => other.to_string(),
    }
}

/// Ask the desktop `message` as a frame and read the frame it answers.
fn ask(desktop: &mut Desktop, message: RemoteActionMessage) -> Result<RemoteActionMessage, String> {
    let frame =
        serde_json::to_value(RemoteActionFrame::new(message)).map_err(|error| error.to_string())?;
    let answer = desktop
        .request(&frame)
        .map_err(|why| unreachable_sentence(&why))?;
    if let Some(err) = answer.get("err") {
        return Err(err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("That didn't work.")
            .to_owned());
    }
    serde_json::from_value::<RemoteActionFrame>(answer)
        .map(|frame| frame.message)
        .map_err(|error| format!("The desktop answered something this can't read: {error}"))
}

fn connect(sockets: &[ControlSocket]) -> Result<Desktop, String> {
    Desktop::connect(sockets).map_err(|why| unreachable_sentence(&why))
}

/// The actions the desktop offers that `filter` mentions: the first of its
/// words narrows the listing the desktop answers, and each row then holds
/// every word.
fn offered(desktop: &mut Desktop, filter: &str, spelling: &Spelling) -> Result<Vec<Offer>, String> {
    let words: Vec<String> = filter
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut query = CatalogueQuery {
        needle: words.first().cloned().unwrap_or_default(),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    };
    let mut rows = Vec::new();
    for _ in 0..LIST_PAGES {
        let page = match ask(
            desktop,
            RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: query.clone(),
            },
        )? {
            RemoteActionMessage::CatalogueResponse { result, .. } => {
                result.map_err(|error| refused(&error, spelling))?
            }
            _ => return Err("The desktop answered something else.".into()),
        };
        rows.extend(
            page.rows
                .into_iter()
                .filter(|row| mentions(row, &words))
                .filter_map(Offer::of),
        );
        if page.exhausted || page.next_cursor.is_empty() {
            break;
        }
        query.revision = Some(page.revision);
        query.cursor = page.next_cursor;
    }
    Ok(rows)
}

/// One action the desktop offers, of the resource it is listed under.
struct Offer {
    provider: String,
    resource: CatalogueResource,
    action: ActionDescriptor,
}

impl Offer {
    /// What `row` offers, when it lists an action.
    fn of(row: CatalogueRow) -> Option<Self> {
        Some(Self {
            action: row.action?,
            provider: row.provider,
            resource: row.resource,
        })
    }
}

/// Whether `row` mentions every one of `words`.
fn mentions(row: &CatalogueRow, words: &[String]) -> bool {
    let Some(action) = row.action.as_ref() else {
        return false;
    };
    let said = [
        action.action_label.as_str(),
        action.address.action.as_str(),
        row.resource.label.as_str(),
        row.resource.id.as_str(),
        row.app_id.as_str(),
    ]
    .map(str::to_lowercase);
    words
        .iter()
        .all(|word| said.iter().any(|text| text.contains(word.as_str())))
}

/// Call the workspace's own action `action` on its resource `child` (`trust`, `registries`),
/// at that address among what the desktop offers, with `args`: what it answered.
///
/// # Errors
/// No desktop answered, it offers no such action, or the action failed.
pub fn call_workspace(
    sockets: &[ControlSocket],
    (child, action): (&str, &str),
    args: &Value,
) -> Result<Value, String> {
    let spelling = Spelling::standalone();
    let mut desktop = connect(sockets)?;
    let at = format!("/{child}");
    let row = offered(&mut desktop, child, &spelling)?
        .into_iter()
        .find(|row| {
            // The workspace's own resource is `<kind>:<workspace>/<child>`.
            let address = &row.action.address;
            address.action == action
                && address
                    .resource
                    .split_once(':')
                    .is_some_and(|(_, rest)| rest.ends_with(&at))
        })
        .ok_or_else(|| format!("This desktop offers no workspace {child}#{action}."))?;
    let args = args.as_object().cloned().unwrap_or_default();
    invoke(&mut desktop, target(&row), args, (action, &spelling))
}

/// What `row` invokes.
fn target(row: &Offer) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: row.provider.clone(),
        address: row.action.address.clone(),
    }
}

/// Do the desktop's own action `target` with `args`, called `title` where the answer names it:
/// a verb `tilo` spells itself, which needs no listing to find. Tilo's API answer (`status`, `invocation`, `result`), or the
/// sentence that says why there is none.
///
/// # Errors
/// What a person reads when the action could not start, failed or was
/// cancelled.
pub fn call_target(
    sockets: &[ControlSocket],
    target: RemoteActionTarget,
    title: &str,
    args: &Value,
    spelling: &Spelling,
) -> Result<Value, String> {
    let args = args.as_object().cloned().unwrap_or_default();
    connect(sockets).and_then(|mut desktop| invoke(&mut desktop, target, args, (title, spelling)))
}

/// Invoke `target` with `args` and follow it until it ends: its answer as
/// Tilo's API answers an action, or the sentence that says why there is none.
/// An action that takes longer than one wait is followed on: a command that
/// printed "still going" and exited as if it had worked would leave the
/// person, or the script after it, without its answer.
///
/// Where the workspace asks the person first, the call waits in its asked record for their
/// answer on their session or another device of theirs: this command is a program of theirs,
/// which never answers its own question, and says once where it waits.
fn invoke(
    desktop: &mut Desktop,
    target: RemoteActionTarget,
    args: Map<String, Value>,
    (title, spelling): (&str, &Spelling),
) -> Result<Value, String> {
    let invocation = InvocationId::new();
    let argument = if args.is_empty() {
        InvokeArgument::None
    } else {
        InvokeArgument::Json(args)
    };
    let request = InvokeRequest {
        asks: false,
        invocation,
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(RUN_WAIT_MS),
        appearance: None,
        signed: None,
    };
    let mut status = match ask(desktop, RemoteActionMessage::Invoke(request))? {
        RemoteActionMessage::Receipt(receipt) => receipt.status,
        _ => return Err("The desktop answered something else.".into()),
    };
    let mut told = false;
    while matches!(status, ReceiptStatus::Accepted) {
        let observed = ask(
            desktop,
            RemoteActionMessage::Observe {
                correlation: 1,
                invocation,
                wait_ms: Some(RUN_WAIT_MS),
                seen: None,
            },
        )?;
        let RemoteActionMessage::Update {
            state: Some(state), ..
        } = observed
        else {
            return Err(format!("The desktop lost track of “{title}”."));
        };
        if state.read_answered
            && let InvocationUpdate::Item { value, .. } = &state.update
        {
            return Ok(serde_json::json!({
                "status": "ok", "invocation": invocation, "result": value,
            }));
        }
        if !told
            && let InvocationUpdate::Progress {
                phase,
                message: Some(record),
                ..
            } = &state.update
            && phase == tilo_actions::remote::asked::ASKED_PHASE
        {
            told = true;
            eprintln!("tilo: waiting for your answer on a device of yours: it waits in {record}");
        }
        status = state.receipt().status;
    }
    match status {
        ReceiptStatus::Completed { output } => {
            let mut typed = serde_json::json!({"status": "ok", "invocation": invocation});
            if let Some(fields) = typed.as_object_mut() {
                if let Some(value) = output.value.filter(|value| !value.is_null()) {
                    fields.insert("result".into(), value);
                }
                if !output.resources.is_empty() {
                    fields.insert("resources".into(), output.resources.into());
                }
            }
            Ok(typed)
        }
        ReceiptStatus::Failed { error } if error.message.trim().is_empty() => {
            Err(format!("“{title}” didn't work."))
        }
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Rejected { error } => Err(refused(&error, spelling)),
        ReceiptStatus::Cancelled { .. } => Err(format!("“{title}” was cancelled.")),
        ReceiptStatus::Accepted => Err(format!("“{title}” is still going.")),
    }
}

/// Say `answered`: the answer as [`answer_text`] words it, or as typed JSON
/// when `json`; what went wrong on standard error, in words either way.
pub fn say(answered: Result<Value, String>, json: bool, io: &mut Io<'_>) -> u8 {
    match answered {
        Ok(answer) if json => {
            let _ = writeln!(
                io.out,
                "{}",
                serde_json::to_string_pretty(&answer).unwrap_or_default()
            );
            0
        }
        Ok(answer) => {
            let _ = writeln!(io.out, "{}", answer_text(&answer));
            0
        }
        Err(sentence) => said(Err(sentence), io),
    }
}

/// Nothing to say once it worked; the sentence when it did not.
fn said(answered: Result<(), String>, io: &mut Io<'_>) -> u8 {
    match answered {
        Ok(()) => 0,
        Err(sentence) => {
            let _ = writeln!(io.err, "{sentence}");
            1
        }
    }
}

/// An action's answer, as Tilo's API gives it, in the lines a person reads:
/// the links it answered first, how it stands and the resource it acted on, what it answered
/// (each list of records a table, each other field a `name: value` line, in the order it gave
/// them), and what it made or changed.
#[must_use]
pub fn answer_text(answer: &Value) -> String {
    use std::fmt::Write as _;
    let text = |name: &str| answer.get(name).and_then(Value::as_str).unwrap_or_default();
    let path = text("path");
    let result = answer.get("result").filter(|result| !result.is_null());
    let links_of = |value: Option<&Value>| -> Vec<String> {
        let Some(value) = value else {
            return Vec::new();
        };
        let mut found: Vec<String> = value
            .get("links")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(value.get("link"))
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        found.dedup();
        found
    };
    let mut lines = links_of(Some(answer));
    lines.extend(links_of(result));
    // An action names the resource it resolved to; one that names none says
    // its status alone.
    let mut said = match text("status") {
        "pending" => {
            let mut said = format!("pending {}", text("invocation"));
            match answer.pointer("/ask/url").and_then(Value::as_str) {
                Some(url) => {
                    let _ = write!(said, ": it waits for you, at {url}");
                }
                None => said.push_str(": still going"),
            }
            if !path.is_empty() {
                let _ = write!(said, " ({path})");
            }
            said
        }
        status if path.is_empty() => status.to_owned(),
        status => format!("{status} {path}"),
    };
    match result {
        None => {}
        Some(Value::Object(fields)) => {
            for (name, value) in fields {
                if matches!(name.as_str(), "link" | "links") {
                    continue;
                }
                match records(value) {
                    Some(rows) => {
                        let _ = write!(said, "\n{name}:\n{}", table(rows));
                    }
                    None => {
                        let _ = write!(said, "\n{name}: {}", scalar(value));
                    }
                }
            }
        }
        Some(value) => match records(value) {
            Some(rows) => {
                let _ = write!(said, "\n{}", table(rows));
            }
            None => {
                let _ = write!(said, "\n{}", scalar(value));
            }
        },
    }
    // What the action answered it made or changed, as the workspace names it:
    // the path the next command can take as it is.
    for resource in answer
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let _ = write!(said, "\n-> {resource}");
    }
    lines.push(said);
    lines.join("\n")
}

/// `value` as a list of records, when it is one: an array whose every item is an object.
fn records(value: &Value) -> Option<&[Value]> {
    let rows = value.as_array()?;
    (!rows.is_empty() && rows.iter().all(Value::is_object)).then_some(rows.as_slice())
}

/// A value as one cell or line reads it: text as it is, anything else as JSON.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `rows` as a table: a column for each field any of them has, in the order first seen, under
/// a header, each column as wide as its widest cell.
fn table(rows: &[Value]) -> String {
    let mut columns: Vec<&str> = Vec::new();
    for row in rows.iter().filter_map(Value::as_object) {
        for name in row.keys() {
            if !columns.contains(&name.as_str()) {
                columns.push(name);
            }
        }
    }
    let cells: Vec<Vec<String>> =
        std::iter::once(columns.iter().map(|c| (*c).to_owned()).collect())
            .chain(rows.iter().map(|row| {
                columns
                    .iter()
                    .map(|column| row.get(*column).map(scalar).unwrap_or_default())
                    .collect()
            }))
            .collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|at| {
            cells
                .iter()
                .filter_map(|row| row.get(at))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    cells
        .iter()
        .map(|row| {
            let line: Vec<String> = row
                .iter()
                .zip(&widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect();
            format!("  {}", line.join("  ").trim_end())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `item` is a link (`https://…`, `mailto:…`) rather than a path.
fn is_link(item: &str) -> bool {
    let Some((scheme, rest)) = item.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && scheme.len() > 1
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A file named relative to `cwd`, made absolute; a link as it is.
#[must_use]
pub fn open_item(item: &str, cwd: &Path) -> String {
    if is_link(item) && !cwd.join(item).exists() {
        return item.to_string();
    }
    let path = Path::new(item);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    absolute.to_string_lossy().into_owned()
}

/// The app `row` opens, when it is the workspace's open of an app: its id.
fn opens(row: &Offer) -> Option<&str> {
    let offered = &row.action;
    (offered.address.action == APPLICATION_OPEN_ACTION
        && provider_workspace(&row.provider).is_some())
    .then(|| {
        offered
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
    })
    .flatten()
}

/// The apps among `rows`: each one's id, with the name it is listed by.
fn apps(rows: &[Offer]) -> Vec<(&str, &str, &Offer)> {
    rows.iter()
        .filter_map(|row| Some((opens(row)?, row.resource.label.as_str(), row)))
        .collect()
}

/// `tilo open APP [file|link…]`: open an app by id, name or the start of its
/// name, as search and Enter do; with files or links, open those in it.
pub fn open(
    sockets: &[ControlSocket],
    app: &str,
    items: &[String],
    cwd: &Path,
    spelling: &Spelling,
    io: &mut Io<'_>,
) -> u8 {
    let mut desktop = match connect(sockets) {
        Ok(desktop) => desktop,
        Err(sentence) => return said(Err(sentence), io),
    };
    let rows = match offered(&mut desktop, app, spelling) {
        Ok(rows) => rows,
        Err(sentence) => return said(Err(sentence), io),
    };
    let apps = apps(&rows);
    let listed: Vec<(&str, &str)> = apps.iter().map(|(id, label, _)| (*id, *label)).collect();
    let chosen = match app_meant(app, &listed) {
        Ok(Some(id)) => apps.iter().find(|(listed, _, _)| *listed == id),
        Ok(None) => None,
        Err(sentence) => return said(Err(sentence), io),
    };
    let Some(&(id, label, row)) = chosen else {
        let _ = writeln!(io.err, "There is no app called “{app}”.");
        return 1;
    };
    let (target, args) = if items.is_empty() {
        (target(row), Map::new())
    } else {
        let workspace = provider_workspace(&row.provider).unwrap_or_default();
        let items: Vec<String> = items.iter().map(|item| open_item(item, cwd)).collect();
        let mut args = Map::new();
        args.insert("open".into(), items.into());
        (
            application_open_target(workspace, &format!("{OPEN_WITH_PREFIX}{id}")),
            args,
        )
    };
    said(
        invoke(&mut desktop, target, args, (label, spelling)).map(drop),
        io,
    )
}

/// App names that complete `prefix` after `tilo open`.
#[must_use]
pub fn complete_apps(sockets: &[ControlSocket], prefix: &str) -> Vec<String> {
    let Ok(mut desktop) = Desktop::connect(sockets) else {
        return Vec::new();
    };
    let Ok(rows) = offered(&mut desktop, prefix, &Spelling::tilo()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = apps(&rows)
        .into_iter()
        .map(|(_, label, _)| label.to_lowercase().replace(char::is_whitespace, "-"))
        .filter(|name| !name.is_empty())
        .collect();
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

#[cfg(test)]
#[path = "front_tests.rs"]
mod tests;
