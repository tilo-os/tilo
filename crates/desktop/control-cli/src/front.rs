//! What a person types: open an app, list what the desktop can do, run one
//! of those things.
//!
//! `tilo open`, `tilo action …`, `tilo do`, the standalone `action` and
//! `app open` are all this module. Each sends the action model's own frames
//! ([`RemoteActionFrame`]) through the first socket that serves this process
//! ([`crate::client`]): the device admin's socket at their own login, or the
//! session's MCP server from inside an app such as Terminal, which acts with
//! that app's workspace scope. Nothing here picks authority or a workspace;
//! the desktop decides both.
//!
//! An action is named by its id (`new-window`), or its title
//! (`new-terminal-window`), as the published tree picks them
//! ([`tilo_actions::tree::pick_action`]); `place#action` names one where
//! several places offer an action of that name.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
    application_open_target, provider_workspace,
};
use tilo_actions::tree::{action_rank, app_meant, bind_choices, folded, pick_action};
use tilo_actions::{ActionDescriptor, InvocationId, InvocationUpdate};

use crate::client::{ControlSocket, Desktop, Unreachable};

/// How long one wait for an invocation lasts: the desktop holds the answer
/// until the action settles or this runs out, and it is asked again.
const RUN_WAIT_MS: u32 = 5_000;

/// Pages one listing reads at most.
const LIST_PAGES: usize = 8;

/// Candidates shown when a name fits more than one action.
const AMBIGUOUS_SHOWN: usize = 8;

/// The installed-app address that opens files and links in an app.
const OPEN_WITH_PREFIX: &str = "open-with:";

/// How this program's commands are spelled in the examples it prints:
/// `tilo do` and `tilo open`, or `action run` and `app open`.
#[derive(Clone, Debug)]
pub struct Spelling {
    pub run: String,
    pub show: String,
    pub list: String,
    pub open: String,
}

impl Spelling {
    /// `tilo do`, `tilo action show`, `tilo action list`, `tilo open`.
    #[must_use]
    pub fn tilo() -> Self {
        Self {
            run: "tilo do".into(),
            show: "tilo action show".into(),
            list: "tilo action list".into(),
            open: "tilo open".into(),
        }
    }

    /// The standalone `action` and `app` commands.
    #[must_use]
    pub fn standalone() -> Self {
        Self {
            run: "action run".into(),
            show: "action show".into(),
            list: "action list".into(),
            open: "app open".into(),
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
        Unreachable::NoDesktop => "This needs a Tilo desktop session.".into(),
        Unreachable::Refused {
            socket: ControlSocket::Agent(_),
            ..
        } => "The desktop didn't let this terminal use its actions.".into(),
        Unreachable::Refused { .. } => {
            "The desktop didn't let this program use its actions.".into()
        }
        Unreachable::Io { .. } => "The desktop stopped answering. Try again.".into(),
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

/// What to list to find the action `name` names: its own id, without the place before it.
fn listed(name: &str) -> &str {
    name.rsplit('#').next().unwrap_or(name)
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
    app_id: String,
    resource: CatalogueResource,
    action: ActionDescriptor,
}

impl Offer {
    /// What `row` offers, when it lists an action.
    fn of(row: CatalogueRow) -> Option<Self> {
        Some(Self {
            action: row.action?,
            provider: row.provider,
            app_id: row.app_id,
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

/// Where `row` acts, as a person names it: its resource's label, or its
/// app's.
fn place(row: &Offer) -> String {
    let label = row.resource.label.trim();
    let named = if label.is_empty() {
        row.app_id.rsplit('.').next().unwrap_or(&row.app_id)
    } else {
        label
    };
    named.to_lowercase().replace(char::is_whitespace, "-")
}

/// Whether `row` acts at the place `name` names: its resource, by label or
/// id, or its app.
fn at(row: &Offer, name: &str) -> bool {
    let wanted = folded(name);
    row.resource.id == name
        || folded(&row.resource.label) == wanted
        || folded(&row.app_id) == wanted
        || folded(row.app_id.rsplit('.').next().unwrap_or_default()) == wanted
}

/// The name `row` is typed by among `rows`: its action's id where no other
/// row offers one of that id, else that and its place.
fn name_of(row: &Offer, rows: &[Offer]) -> String {
    let id = &row.action.address.action;
    let shared = rows
        .iter()
        .filter(|other| other.action.address.action == *id)
        .count();
    if shared > 1 {
        format!("{}#{id}", place(row))
    } else {
        id.clone()
    }
}

/// Where `row` stands, as a full address names it: its provider and its
/// resource, so two rows whose place reads alike are told apart.
fn full_place(row: &Offer) -> String {
    format!("{}:{}", row.provider, row.action.address.resource)
}

/// The full address of the action `row` offers: its provider, resource and
/// action, the one name that finds it alone.
fn full_name(row: &Offer) -> String {
    format!("{}#{}", full_place(row), row.action.address.action)
}

/// The row `name` picks among `rows`: a full address
/// (`provider:resource#action`), `place#action`, or an action by its id or
/// its title, as the published tree picks one.
fn find<'a>(rows: &'a [Offer], name: &str, spelling: &Spelling) -> Result<&'a Offer, String> {
    let (within, wanted) = match name.rsplit_once('#') {
        Some((place, wanted)) => (Some(place), wanted),
        None => (None, name),
    };
    let here: Vec<&Offer> = match within {
        // A provider and a resource name one row's place, however several
        // others read alike.
        Some(within) if rows.iter().any(|row| full_place(row) == within) => rows
            .iter()
            .filter(|row| full_place(row) == within)
            .collect(),
        _ => rows
            .iter()
            .filter(|row| within.is_none_or(|within| at(row, within)))
            .collect(),
    };
    let actions: Vec<ActionDescriptor> = here.iter().map(|row| row.action.clone()).collect();
    let picked = pick_action(wanted, &actions, "this desktop")
        .or_else(|_| pick_action(&wanted.replace('-', " "), &actions, "this desktop"))
        .map_err(|_| {
            format!(
                "Nothing here is called “{name}”. See what this desktop can do: {}",
                spelling.list
            )
        })?;
    let rank = action_rank(wanted, picked);
    let mut fits: Vec<&Offer> = here
        .iter()
        .copied()
        .filter(|row| {
            let offered = &row.action;
            if rank.is_some() {
                action_rank(wanted, offered) == rank
            } else {
                offered.action_label == picked.action_label
            }
        })
        .collect();
    // One action has one entry: a row a provider offers twice, or the same
    // action published under one provider more than once, is read once.
    let mut seen = HashSet::new();
    fits.retain(|row| {
        seen.insert((
            row.provider.clone(),
            row.action.address.resource.clone(),
            row.action.address.action.clone(),
        ))
    });
    match fits.as_slice() {
        [one] => Ok(one),
        [] => Err("That didn't work.".into()),
        several => {
            use std::fmt::Write as _;
            let mut text = format!("“{name}” could be more than one thing. Say which:");
            // Each suggestion is a full address, so what is typed finds the
            // one row it names rather than asking again.
            for row in several.iter().take(AMBIGUOUS_SHOWN) {
                let _ = write!(
                    text,
                    "\n  {} {}    {}",
                    spelling.run,
                    full_name(row),
                    row.action.action_label
                );
            }
            Err(text)
        }
    }
}

/// `tilo action list [filter]`: what this desktop can do.
pub fn list(sockets: &[ControlSocket], filter: &str, spelling: &Spelling, io: &mut Io<'_>) -> u8 {
    let mut desktop = match Desktop::connect(sockets) {
        Ok(desktop) => desktop,
        // A desktop is where this desktop's actions are; a hosted workspace
        // lists what it holds with `tilo ls`, which is the command to reach
        // instead when there is no session here.
        Err(Unreachable::NoDesktop) => {
            let _ = writeln!(
                io.err,
                "This needs a Tilo desktop session. In a hosted workspace, `tilo ls -w <workspace>` \
                 lists what it holds."
            );
            return 1;
        }
        Err(why) => return said(Err(unreachable_sentence(&why)), io),
    };
    let rows = match offered(&mut desktop, filter.trim(), spelling) {
        Ok(rows) => rows,
        Err(sentence) => return said(Err(sentence), io),
    };
    if rows.is_empty() {
        let _ = if filter.trim().is_empty() {
            writeln!(io.out, "This desktop offers nothing right now.")
        } else {
            writeln!(io.out, "Nothing here matches “{}”.", filter.trim())
        };
        return 0;
    }
    let names: Vec<String> = rows.iter().map(|row| name_of(row, &rows)).collect();
    let width = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0);
    for (row, name) in rows.iter().zip(&names) {
        let line = format!(
            "{name:<width$}  {}  {}",
            row.action.action_label, row.resource.label
        );
        let _ = writeln!(io.out, "{}", line.trim_end());
    }
    0
}

/// `tilo action show NAME`: what an action does, where, and what it takes.
pub fn show(sockets: &[ControlSocket], name: &str, spelling: &Spelling, io: &mut Io<'_>) -> u8 {
    let rows = match connect(sockets)
        .and_then(|mut desktop| offered(&mut desktop, listed(name), spelling))
    {
        Ok(rows) => rows,
        Err(sentence) => return said(Err(sentence), io),
    };
    let row = match find(&rows, name, spelling) {
        Ok(row) => row,
        Err(sentence) => return said(Err(sentence), io),
    };
    let offered = &row.action;
    let display = name_of(row, &rows);
    let _ = writeln!(io.out, "{}", offered.action_label);
    if !row.resource.label.is_empty() {
        let _ = writeln!(io.out, "{}", row.resource.label);
    }
    let _ = writeln!(io.out);
    let _ = writeln!(io.out, "Name      {display}");
    let _ = writeln!(
        io.out,
        "Address   {} {}#{}",
        row.provider, offered.address.resource, offered.address.action
    );
    if offered.params.is_empty() {
        let _ = writeln!(io.out, "Takes     nothing");
    } else {
        let _ = writeln!(io.out, "Takes");
        let width = offered
            .params
            .iter()
            .map(|param| param.name.chars().count())
            .max()
            .unwrap_or(0);
        for param in &offered.params {
            let need = if param.required {
                "required"
            } else {
                "optional"
            };
            let mut line = format!("  {:<width$}  {}, {need}", param.name, param.ty.label());
            if !param.description.is_empty() && param.description != param.name {
                line.push_str("  ");
                line.push_str(&param.description);
            }
            let _ = writeln!(io.out, "{line}");
        }
    }
    let mut example = String::new();
    for param in offered.params.iter().filter(|param| param.required) {
        example.push(' ');
        example.push_str(&param.name);
        example.push_str("=…");
    }
    let _ = writeln!(io.out, "\nExample\n  {} {display}{example}", spelling.run);
    0
}

/// Turn what was typed after an action's name into its arguments:
/// `name=value`, `--name=value` and `--name value` name a parameter, each
/// value the JSON it reads as or else text; a lone value goes to the one
/// parameter that is required, or the only one, or the one of a type it can
/// be (`dark` is no object). The desktop checks them against what the action
/// takes.
///
/// # Errors
/// A sentence saying what was wrong and what the action takes.
fn bind_args(offered: &ActionDescriptor, words: &[String]) -> Result<Map<String, Value>, String> {
    let takes = || {
        if offered.params.is_empty() {
            "nothing".to_owned()
        } else {
            offered
                .params
                .iter()
                .map(|param| format!("{} ({})", param.name, param.ty.label()))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut args = Map::new();
    let mut bare: Vec<&str> = Vec::new();
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        let named = |key: &str| offered.params.iter().any(|param| param.name == key);
        if let Some(flag) = word.strip_prefix("--") {
            let (key, value) = match flag.split_once('=') {
                Some((key, value)) => (key.to_owned(), value.to_owned()),
                None => (
                    flag.to_owned(),
                    iter.next()
                        .ok_or_else(|| format!("--{flag} needs a value after it."))?
                        .clone(),
                ),
            };
            args.insert(key, read_value(&value));
        } else if let Some((key, value)) = word.split_once('=')
            && named(key)
        {
            args.insert(key.to_owned(), read_value(value));
        } else {
            bare.push(word);
        }
    }
    if !bare.is_empty() {
        let open: Vec<_> = offered
            .params
            .iter()
            .filter(|param| param.required && !args.contains_key(param.name.as_ref()))
            .collect();
        let value = read_value(&bare.join(" "));
        let lone = match (open.as_slice(), offered.params.as_slice()) {
            ([one], _) => Some(*one),
            ([], [only]) if !args.contains_key(only.name.as_ref()) => Some(only),
            ([], params) => {
                let fitting: Vec<_> = params
                    .iter()
                    .filter(|param| {
                        !args.contains_key(param.name.as_ref()) && can_be(param.ty, &value)
                    })
                    .collect();
                match fitting.as_slice() {
                    [one] => Some(*one),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(lone) = lone else {
            return Err(format!(
                "Say which option “{}” is for: “{}” takes {}.",
                bare.join(" "),
                offered.action_label,
                takes()
            ));
        };
        args.insert(lone.name.to_string(), value);
    }
    bind_choices(offered, &mut args);
    Ok(args)
}

/// Whether a value typed alone could be one of type `ty`: a word is never an object, a list,
/// a number or a yes-or-no unless it reads as one.
fn can_be(ty: tilo_actions::ParamType, value: &Value) -> bool {
    use tilo_actions::ParamType;
    match ty {
        ParamType::Object => value.is_object(),
        ParamType::Array => value.is_array(),
        ParamType::Integer => value.is_i64() || value.is_u64(),
        ParamType::Number => value.is_number(),
        ParamType::Boolean => value.is_boolean(),
        ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret => true,
    }
}

/// A value as typed: the JSON it reads as (`3`, `true`, `[1]`), else text.
fn read_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// The value of an option written `name=@-`: read from standard input, as text, so a secret
/// (an API key) never stands in a command line, where every process on the machine can read
/// it.
pub const FROM_INPUT: &str = "@-";

/// Arguments for an action whose parameters this side has not seen: each
/// `name=value` (or `--name=value`), its value as the JSON it reads as, else
/// as text (`count=3` is a number, `title=Groceries` text); `name=@-` reads
/// that one value from standard input ([`FROM_INPUT`]). Whoever carries out
/// the action checks them against what it takes.
///
/// # Errors
/// The sentence that names a word that is no `name=value`, or a value that
/// could not be read from standard input.
pub fn arguments(words: &[String]) -> Result<Map<String, Value>, String> {
    arguments_reading(words, &mut std::io::stdin().lock())
}

/// [`arguments`], a `name=@-` value read from `input`.
///
/// # Errors
/// As [`arguments`]; more than one value named to be read from `input`.
pub fn arguments_reading(
    words: &[String],
    input: &mut dyn std::io::Read,
) -> Result<Map<String, Value>, String> {
    let mut args = Map::new();
    let mut read = false;
    for word in words {
        let Some((key, value)) = word.trim_start_matches("--").split_once('=') else {
            return Err(format!("Write “{word}” as name=value."));
        };
        let value = if value == FROM_INPUT {
            if std::mem::replace(&mut read, true) {
                return Err(format!(
                    "Only one value is read from standard input; “{key}” is a second."
                ));
            }
            let mut text = String::new();
            input.read_to_string(&mut text).map_err(|error| {
                format!("“{key}” could not be read from standard input: {error}")
            })?;
            Value::String(text.trim_end_matches(['\n', '\r']).to_owned())
        } else {
            read_value(value)
        };
        args.insert(key.to_string(), value);
    }
    Ok(args)
}

/// `tilo do NAME [args]`: do one action, waiting for how it went, and say
/// its answer: the lines a person reads, or with `json` the typed answer
/// itself, the one shape `tilo do -w` answers too.
pub fn run(
    sockets: &[ControlSocket],
    name: &str,
    words: &[String],
    json: bool,
    spelling: &Spelling,
    io: &mut Io<'_>,
) -> u8 {
    let answered = connect(sockets).and_then(|mut desktop| {
        let rows = offered(&mut desktop, listed(name), spelling)?;
        let row = find(&rows, name, spelling)?;
        let args = bind_args(&row.action, words)?;
        invoke(
            &mut desktop,
            target(row),
            args,
            (&row.action.action_label, spelling),
            &mut terminal_yes,
        )
    });
    say(answered, json, io)
}

/// Do the action `name` names, with `args`, and answer what it answered: for a
/// program reading a workspace's readings rather than saying one action's
/// lines. `name` is resolved as [`run`] resolves it.
///
/// # Errors
/// The desktop could not be reached, the name is ambiguous or unknown, or the
/// action failed.
pub fn call_action(
    sockets: &[ControlSocket],
    name: &str,
    args: &Value,
    spelling: &Spelling,
) -> Result<Value, String> {
    let mut desktop = connect(sockets)?;
    let rows = offered(&mut desktop, listed(name), spelling)?;
    let row = find(&rows, name, spelling)?;
    let args = args.as_object().cloned().unwrap_or_default();
    invoke(
        &mut desktop,
        target(row),
        args,
        (&row.action.action_label, spelling),
        &mut terminal_yes,
    )
}

/// What `row` invokes.
fn target(row: &Offer) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: row.provider.clone(),
        address: row.action.address.clone(),
    }
}

/// Do the desktop's own action `target` with `args`, called `title` where
/// the answer names it: a verb `tilo` spells itself, which needs no listing
/// to find. Says what it answers, as `tilo
/// do` says an answer.
pub fn run_target(
    sockets: &[ControlSocket],
    target: RemoteActionTarget,
    title: &str,
    args: &Value,
    spelling: &Spelling,
    io: &mut Io<'_>,
) -> u8 {
    say(
        call_target(sockets, target, title, args, spelling),
        false,
        io,
    )
}

/// [`run_target`]'s answer, for a verb that reads what it answers rather than
/// saying it: Tilo's API answer (`status`, `invocation`, `result`), or the
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
    connect(sockets).and_then(|mut desktop| {
        invoke(
            &mut desktop,
            target,
            args,
            (title, spelling),
            &mut terminal_yes,
        )
    })
}

/// Ask the person `question` at this command's own prompt, on the terminal it was typed at:
/// their yes or no, or `None` where nobody is at a terminal to answer (a script, a pipe), and
/// the question waits for them where Tilo shows it instead.
fn terminal_yes(question: &str) -> Option<bool> {
    use std::io::{BufRead as _, IsTerminal as _};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() || !std::io::stderr().is_terminal() {
        return None;
    }
    let mut err = std::io::stderr();
    let _ = write!(err, "{question}");
    let _ = err.flush();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok()?;
    Some(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// The question `tilo do` asks at its own prompt for the call `title` with `args`, which the
/// workspace asks the person first: what it does, with what, and how to answer.
fn confirm_question(title: &str, args: &Map<String, Value>) -> String {
    let with: Vec<String> = args
        .iter()
        .map(|(name, value)| match value {
            Value::String(text) => format!("{name}={text}"),
            other => format!("{name}={other}"),
        })
        .collect();
    if with.is_empty() {
        format!("“{title}” asks for your yes first. Go ahead? [y/N] ")
    } else {
        format!(
            "“{title}” ({}) asks for your yes first. Go ahead? [y/N] ",
            with.join(", ")
        )
    }
}

/// Answer the asked record `record` (`<workspace>/<record>`, as the call's progress names it)
/// on `desktop` with the person's `yes` or no: its own `resolve`, on this command's connection,
/// which the daemon admits as the person's confirmation only where the kernel says the
/// connection is theirs.
fn answer_asked(desktop: &mut Desktop, record: &str, yes: bool) -> Result<(), String> {
    let record = record.split_once('/').map_or(record, |(_, record)| record);
    let mut answer = Map::new();
    answer.insert(
        "resolution".into(),
        Value::from(if yes { "approve" } else { "decline" }),
    );
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::WORKSPACE_INVOCATIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(record, tilo_actions::remote::asked::RESOLVE),
        },
        argument: InvokeArgument::Json(answer),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(RUN_WAIT_MS),
        appearance: None,
        signed: None,
    };
    match ask(desktop, RemoteActionMessage::Invoke(request))? {
        RemoteActionMessage::Receipt(receipt) => match receipt.status {
            ReceiptStatus::Rejected { error } => Err(error.to_string()),
            ReceiptStatus::Failed { error } => Err(error.message),
            _ => Ok(()),
        },
        _ => Err("The desktop answered something else.".into()),
    }
}

/// Invoke `target` with `args` and follow it until it ends: its answer as
/// Tilo's API answers an action, or the sentence that says why there is none.
/// An action that takes longer than one wait is followed on: a command that
/// printed "still going" and exited as if it had worked would leave the
/// person, or the script after it, without its answer.
///
/// Where the workspace asks the person first, the call waits in its asked record: `prompt`
/// asks them at this command's own prompt, once, and their answer is that record's. Making the
/// call is never the answer.
fn invoke(
    desktop: &mut Desktop,
    target: RemoteActionTarget,
    args: Map<String, Value>,
    (title, spelling): (&str, &Spelling),
    prompt: &mut dyn FnMut(&str) -> Option<bool>,
) -> Result<Value, String> {
    let invocation = InvocationId::new();
    let question = confirm_question(title, &args);
    let argument = if args.is_empty() {
        InvokeArgument::None
    } else {
        InvokeArgument::Json(args)
    };
    let request = InvokeRequest {
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
    let mut prompted = false;
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
        if !prompted
            && let InvocationUpdate::Progress {
                phase,
                message: Some(record),
                ..
            } = &state.update
            && phase == tilo_actions::remote::asked::ASKED_PHASE
        {
            prompted = true;
            if let Some(yes) = prompt(&question) {
                answer_asked(desktop, record, yes)?;
            }
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
        invoke(
            &mut desktop,
            target,
            args,
            (label, spelling),
            &mut terminal_yes,
        )
        .map(drop),
        io,
    )
}

/// Names that complete `prefix` after `tilo do`.
#[must_use]
pub fn complete_actions(sockets: &[ControlSocket], prefix: &str) -> Vec<String> {
    let Ok(mut desktop) = Desktop::connect(sockets) else {
        return Vec::new();
    };
    let Ok(rows) = offered(&mut desktop, prefix, &Spelling::tilo()) else {
        return Vec::new();
    };
    rows.iter().map(|row| name_of(row, &rows)).collect()
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
