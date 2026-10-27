//! What a person types: open an app, list what the desktop can do, run one
//! of those things.
//!
//! `tilo open`, `tilo action …`, `tilo do`, the standalone `action` and
//! `app open` are all this module, over the one control protocol
//! ([`crate::client`]). Each request goes through the first door that serves
//! this process: the device admin's socket at their own login, or the
//! session's agent door from inside an app such as Terminal, which acts with
//! that app's workspace scope. Nothing here picks authority; the desktop
//! decides what each door may do.
//!
//! Actions are named for people: the slug of their title
//! (`new-terminal-window`), made unique within a listing by the place they
//! act on or a short tag. A name is matched again on every call, and words
//! of a title find it too, so `tilo do "new terminal"` works.

use std::io::Write;
use std::path::Path;

use crate::client::{Desktop, Door, Unreachable};

/// How long an invocation is waited for before its answer: the desktop
/// holds the reply until the action settles or this runs out.
const RUN_WAIT_MS: u64 = 5_000;

/// Rows one listing asks for.
const LIST_LIMIT: usize = 200;

/// Candidates shown when a name fits more than one action.
const AMBIGUOUS_SHOWN: usize = 8;

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
    /// `tilo do`, `tilo action show`, `tilo actions`, `tilo open`.
    #[must_use]
    pub fn tilo() -> Self {
        Self {
            run: "tilo do".into(),
            show: "tilo action show".into(),
            list: "tilo actions".into(),
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

/// One parameter an action takes.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub description: String,
    pub choices: Vec<String>,
    pub nullable: bool,
}

/// One row of the desktop's listing, as a person reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub provider: String,
    pub id: String,
    pub kind: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub params: Vec<Param>,
}

impl Action {
    fn from_row(row: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| row.get(key).and_then(serde_json::Value::as_str);
        let params = row
            .get("params")
            .and_then(serde_json::Value::as_array)
            .map(|params| params.iter().filter_map(Param::from_row).collect())
            .unwrap_or_default();
        Some(Self {
            provider: text("provider")?.to_string(),
            id: text("id")?.to_string(),
            kind: text("kind").unwrap_or_default().to_string(),
            title: text("title").unwrap_or_default().to_string(),
            subtitle: text("subtitle")
                .map(str::trim)
                .filter(|subtitle| !subtitle.is_empty())
                .map(str::to_string),
            params,
        })
    }

    /// Whether it opens an app rather than acting inside one.
    fn is_application(&self) -> bool {
        self.kind == "Apps"
    }

    /// The names it answers to: its title's slug, that and where it acts,
    /// and that and a tag of its address.
    fn names(&self) -> [String; 3] {
        let base = slug(&self.title);
        let base = if base.is_empty() {
            slug(&self.id)
        } else {
            base
        };
        let place = self
            .subtitle
            .as_deref()
            .map(slug)
            .filter(|place| !place.is_empty())
            .map_or_else(|| base.clone(), |place| format!("{base}-{place}"));
        let tag = format!("{base}-{}", address_tag(&self.provider, &self.id));
        [base, place, tag]
    }
}

impl Param {
    fn from_row(row: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| row.get(key).and_then(serde_json::Value::as_str);
        Some(Self {
            name: text("name")?.to_string(),
            ty: text("type").unwrap_or("text").to_string(),
            required: row
                .get("required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            description: text("description").unwrap_or_default().trim().to_string(),
            choices: row
                .get("choices")
                .and_then(serde_json::Value::as_array)
                .map(|choices| {
                    choices
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            nullable: row
                .get("nullable")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// `title (text)`, `count (whole number)`, `mode (dark or light)`.
    fn summary(&self) -> String {
        format!("{} ({})", self.name, self.what())
    }

    fn what(&self) -> String {
        if !self.choices.is_empty() {
            return one_of(&self.choices);
        }
        match self.ty.as_str() {
            "integer" => "whole number".into(),
            "number" => "number".into(),
            "boolean" => "yes or no".into(),
            "array" => "list".into(),
            "object" => "JSON object".into(),
            "datetime" => "date and time".into(),
            "resource" => "item".into(),
            _ => "text".into(),
        }
    }
}

fn one_of(choices: &[String]) -> String {
    match choices {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// Lower-case words joined by `-`: `New terminal window` → `new-terminal-window`.
#[must_use]
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Four hex digits of the address, stable for as long as the address is.
fn address_tag(provider: &str, id: &str) -> String {
    // FNV-1a: small, dependency-free and stable across builds.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in provider.bytes().chain([0]).chain(id.bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{:04x}", hash & 0xffff)
}

/// Each action's name in `actions`: the first of its names no other row in
/// the listing shares.
#[must_use]
pub fn display_names(actions: &[Action]) -> Vec<String> {
    let all: Vec<[String; 3]> = actions.iter().map(Action::names).collect();
    all.iter()
        .enumerate()
        .map(|(at, names)| {
            names
                .iter()
                .find(|name| {
                    all.iter()
                        .enumerate()
                        .all(|(other, theirs)| other == at || !theirs.contains(name))
                })
                .or_else(|| names.last())
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// What a typed name picks out of a listing.
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    One(usize),
    Many(Vec<usize>),
    None,
}

/// Pick the action `input` names in `actions`: an address (`provider:id`)
/// exactly, else a name exactly, else the actions whose title holds every
/// word typed.
#[must_use]
pub fn pick(input: &str, actions: &[Action]) -> Pick {
    let input = input.trim();
    if let Some(at) = actions.iter().position(|action| {
        format!(
            "{}:{}",
            crate::client::printable_id(&action.provider),
            crate::client::printable_id(&action.id)
        ) == input
    }) {
        return Pick::One(at);
    }
    let wanted = slug(input);
    let named: Vec<usize> = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| action.names().contains(&wanted))
        .map(|(at, _)| at)
        .collect();
    match named.as_slice() {
        [one] => return Pick::One(*one),
        [] => {}
        _ => return Pick::Many(named),
    }
    let words: Vec<&str> = wanted.split('-').filter(|word| !word.is_empty()).collect();
    if words.is_empty() {
        return Pick::None;
    }
    let fits: Vec<usize> = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| {
            let title = slug(&action.title);
            let title: Vec<&str> = title.split('-').collect();
            words
                .iter()
                .all(|word| title.iter().any(|part| part.starts_with(word)))
        })
        .map(|(at, _)| at)
        .collect();
    match fits.as_slice() {
        [] => Pick::None,
        [one] => Pick::One(*one),
        _ => Pick::Many(fits),
    }
}

/// Turn what was typed after an action's name into its arguments, checked
/// against what it takes. `key=value`, `--key value` and `--key=value` name
/// a parameter; a lone value goes to the one parameter that is required (or
/// the only one); `--json '{…}'` passes an object as it is.
///
/// # Errors
/// A sentence saying what was wrong and what the action takes.
pub fn bind_args(
    action: &Action,
    words: &[String],
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut args = serde_json::Map::new();
    let mut bare: Vec<&str> = Vec::new();
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        if word == "--json" {
            let raw = iter
                .next()
                .ok_or_else(|| "--json needs a JSON object after it.".to_string())?;
            let value: serde_json::Value = serde_json::from_str(raw)
                .map_err(|_| "What follows --json isn't valid JSON.".to_string())?;
            let serde_json::Value::Object(object) = value else {
                return Err("What follows --json must be a JSON object.".into());
            };
            args.extend(object);
            continue;
        }
        if let Some(flag) = word.strip_prefix("--") {
            let (key, value) = if let Some((key, value)) = flag.split_once('=') {
                (key.to_string(), value.to_string())
            } else {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("--{flag} needs a value after it."))?;
                (flag.to_string(), value.clone())
            };
            insert(action, &mut args, &key, &value)?;
        } else if let Some((key, value)) = word.split_once('=')
            && action.params.iter().any(|param| param.name == key)
        {
            insert(action, &mut args, key, value)?;
        } else {
            bare.push(word);
        }
    }
    if !bare.is_empty() {
        let target = lone_target(action, &args).ok_or_else(|| {
            if action.params.is_empty() {
                format!("“{}” takes nothing.", action.title)
            } else {
                format!(
                    "Say which option “{}” is for: {}.",
                    bare.join(" "),
                    takes(action)
                )
            }
        })?;
        let value = bare.join(" ");
        let name = target.name.clone();
        insert(action, &mut args, &name, &value)?;
    }
    if let Some(missing) = action
        .params
        .iter()
        .find(|param| param.required && !args.contains_key(&param.name))
    {
        return Err(format!(
            "“{}” needs {}. It takes {}.",
            action.title,
            missing.summary(),
            takes(action)
        ));
    }
    Ok(args)
}

/// Arguments for an action named by its address, whose parameters this
/// side has not seen: `key=value` as text, `--json '{…}'` as it is. The
/// desktop checks them against what the action takes.
fn bind_loose(words: &[String]) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut args = serde_json::Map::new();
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        if word == "--json" {
            let raw = iter
                .next()
                .ok_or_else(|| "--json needs a JSON object after it.".to_string())?;
            match serde_json::from_str(raw) {
                Ok(serde_json::Value::Object(object)) => args.extend(object),
                _ => return Err("What follows --json must be a JSON object.".into()),
            }
        } else if let Some((key, value)) = word.trim_start_matches("--").split_once('=') {
            args.insert(key.to_string(), value.into());
        } else {
            return Err(format!(
                "Write “{word}” as name=value, or pass --json '{{…}}'."
            ));
        }
    }
    Ok(args)
}

/// The parameter a lone value is for: the one required parameter not yet
/// given, or the only parameter.
fn lone_target<'a>(
    action: &'a Action,
    given: &serde_json::Map<String, serde_json::Value>,
) -> Option<&'a Param> {
    let open: Vec<&Param> = action
        .params
        .iter()
        .filter(|param| param.required && !given.contains_key(&param.name))
        .collect();
    match (open.as_slice(), action.params.as_slice()) {
        ([one], _) => Some(one),
        ([], [only]) if !given.contains_key(&only.name) => Some(only),
        _ => None,
    }
}

fn takes(action: &Action) -> String {
    if action.params.is_empty() {
        return "nothing".into();
    }
    action
        .params
        .iter()
        .map(Param::summary)
        .collect::<Vec<_>>()
        .join(", ")
}

fn insert(
    action: &Action,
    args: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    raw: &str,
) -> Result<(), String> {
    let param = action
        .params
        .iter()
        .find(|param| param.name == key)
        .ok_or_else(|| {
            format!(
                "“{}” has no option “{key}”. It takes {}.",
                action.title,
                takes(action)
            )
        })?;
    let value = coerce(param, raw)?;
    args.insert(param.name.clone(), value);
    Ok(())
}

fn coerce(param: &Param, raw: &str) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    if param.nullable && raw == "null" {
        return Ok(Value::Null);
    }
    if !param.choices.is_empty() && !param.choices.iter().any(|choice| choice == raw) {
        return Err(format!(
            "{} must be {}, not “{raw}”.",
            param.name,
            one_of(&param.choices)
        ));
    }
    let wrong = || format!("{} must be a {}, not “{raw}”.", param.name, param.what());
    match param.ty.as_str() {
        "integer" => raw.parse::<i64>().map(Value::from).map_err(|_| wrong()),
        "number" => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(wrong),
        "boolean" => match raw.to_lowercase().as_str() {
            "yes" | "true" | "on" | "1" => Ok(Value::Bool(true)),
            "no" | "false" | "off" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("{} must be yes or no, not “{raw}”.", param.name)),
        },
        "array" | "object" => {
            let value: Value = serde_json::from_str(raw).map_err(|_| {
                format!(
                    "{} must be a {} written as JSON, not “{raw}”.",
                    param.name,
                    param.what()
                )
            })?;
            let fits = if param.ty == "array" {
                value.is_array()
            } else {
                value.is_object()
            };
            if fits { Ok(value) } else { Err(wrong()) }
        }
        _ => Ok(Value::String(raw.to_string())),
    }
}

fn listing_request(query: &str, kinds: &[&str]) -> serde_json::Value {
    let mut request = serde_json::json!({
        "op": "actions_enumerate",
        "query": query,
        "limit": LIST_LIMIT,
        "detail": "compact",
    });
    if !kinds.is_empty()
        && let Some(object) = request.as_object_mut()
    {
        object.insert("kinds".into(), kinds.to_vec().into());
    }
    request
}

/// A listing's rows and whether more were left out.
struct Listing {
    actions: Vec<Action>,
    total: Option<u64>,
    truncated: bool,
}

fn listing(desktop: &mut Desktop, query: &str, kinds: &[&str]) -> Result<Listing, Failure> {
    let answer = desktop
        .request(&listing_request(query, kinds))
        .map_err(Failure::Unreachable)?;
    if let Some(err) = answer.get("err") {
        return Err(Failure::Refused(err.clone()));
    }
    let answer = answer.get("answer").cloned().unwrap_or_default();
    Ok(Listing {
        actions: answer
            .get("actions")
            .and_then(serde_json::Value::as_array)
            .map(|rows| rows.iter().filter_map(Action::from_row).collect())
            .unwrap_or_default(),
        total: answer.get("total").and_then(serde_json::Value::as_u64),
        truncated: answer
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Why a command could not finish.
enum Failure {
    Unreachable(Unreachable),
    /// The desktop answered `err`.
    Refused(serde_json::Value),
    /// A sentence of our own.
    Said(String),
}

/// The one sentence a person reads when the desktop could not be reached.
#[must_use]
pub fn unreachable_sentence(why: &Unreachable) -> String {
    match why {
        Unreachable::NoDesktop => "This needs a Tilo desktop session.".into(),
        Unreachable::Refused {
            door: Door::Agent(_),
            ..
        } => "The desktop didn't let this terminal use its actions.".into(),
        Unreachable::Refused { .. } => {
            "The desktop didn't let this program use its actions.".into()
        }
        Unreachable::Io { .. } => "The desktop stopped answering. Try again.".into(),
    }
}

fn refused_sentence(err: &serde_json::Value, spelling: &Spelling) -> String {
    let code = err
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let message = err
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    match code {
        "unknown_action" => format!(
            "That isn't something this desktop can do now. See what it can: {}",
            spelling.list
        ),
        "session_locked" => "Unlock the desktop first.".into(),
        "not_permitted" => "You haven't allowed that here.".into(),
        _ if message.is_empty() => "That didn't work.".into(),
        _ => message.to_string(),
    }
}

fn report(failure: &Failure, spelling: &Spelling, io: &mut Io<'_>) -> u8 {
    let sentence = match failure {
        Failure::Unreachable(why) => unreachable_sentence(why),
        Failure::Refused(err) => refused_sentence(err, spelling),
        Failure::Said(sentence) => sentence.clone(),
    };
    let _ = writeln!(io.err, "{sentence}");
    1
}

fn connect(doors: &[Door]) -> Result<Desktop, Failure> {
    Desktop::connect(doors).map_err(Failure::Unreachable)
}

/// `tilo actions [filter]`: what this desktop can do, grouped by kind.
pub fn list(doors: &[Door], filter: &str, spelling: &Spelling, io: &mut Io<'_>) -> u8 {
    let result = connect(doors).and_then(|mut desktop| listing(&mut desktop, filter.trim(), &[]));
    let listing = match result {
        Ok(listing) => listing,
        Err(failure) => return report(&failure, spelling, io),
    };
    if listing.actions.is_empty() {
        let _ = if filter.trim().is_empty() {
            writeln!(io.out, "This desktop offers nothing right now.")
        } else {
            writeln!(io.out, "Nothing here matches “{}”.", filter.trim())
        };
        return 0;
    }
    let names = display_names(&listing.actions);
    let width = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut kinds: Vec<&str> = Vec::new();
    for action in &listing.actions {
        if !kinds.contains(&action.kind.as_str()) {
            kinds.push(&action.kind);
        }
    }
    for (group, kind) in kinds.iter().enumerate() {
        if group > 0 {
            let _ = writeln!(io.out);
        }
        let heading = if kind.is_empty() { "Other" } else { kind };
        let _ = writeln!(io.out, "{heading}");
        for (action, name) in listing.actions.iter().zip(&names) {
            if action.kind != *kind {
                continue;
            }
            let mut line = format!("  {name:<width$}  {}", action.title);
            if let Some(subtitle) = &action.subtitle {
                line.push_str("  ");
                line.push_str(subtitle);
            }
            let _ = writeln!(io.out, "{}", line.trim_end());
        }
    }
    if listing.truncated {
        let shown = listing.actions.len();
        let _ = match listing.total {
            Some(total) => writeln!(
                io.err,
                "\nShowing {shown} of {total}. Add words to narrow it down: {} <words>",
                spelling.list
            ),
            None => writeln!(
                io.err,
                "\nShowing the first {shown}. Add words to narrow it down: {} <words>",
                spelling.list
            ),
        };
    }
    0
}

/// Find the one action `name` picks, listing with its words. An address
/// (`provider:id`, as `show` prints it) is taken as it is: names never hold
/// a colon.
fn find(desktop: &mut Desktop, name: &str, spelling: &Spelling) -> Result<Action, Failure> {
    if let Some((provider, id)) = name.split_once(':')
        && !provider.is_empty()
        && !id.is_empty()
        && !provider.contains(char::is_whitespace)
    {
        return Ok(Action {
            provider: crate::client::id_from_printed(provider),
            id: crate::client::id_from_printed(id),
            kind: String::new(),
            title: name.to_string(),
            subtitle: None,
            params: Vec::new(),
        });
    }
    let listing = listing(desktop, &slug(name).replace('-', " "), &[])?;
    match pick(name, &listing.actions) {
        Pick::One(at) => listing
            .actions
            .get(at)
            .cloned()
            .ok_or_else(|| Failure::Said("That didn't work.".into())),
        Pick::None => Err(Failure::Said(format!(
            "Nothing here is called “{name}”. See what this desktop can do: {}",
            spelling.list
        ))),
        Pick::Many(candidates) => {
            use std::fmt::Write as _;
            let names = display_names(&listing.actions);
            let mut text = format!("“{name}” could be more than one thing. Say which:");
            let shown = candidates
                .iter()
                .take(AMBIGUOUS_SHOWN)
                .filter_map(|at| Some((listing.actions.get(*at)?, names.get(*at)?)));
            for (action, display) in shown {
                let _ = write!(text, "\n  {} {display}    {}", spelling.run, action.title);
                if let Some(subtitle) = &action.subtitle {
                    let _ = write!(text, "  {subtitle}");
                }
            }
            Err(Failure::Said(text))
        }
    }
}

/// `tilo action show NAME`: what an action does, where, and what it takes.
pub fn show(doors: &[Door], name: &str, spelling: &Spelling, io: &mut Io<'_>) -> u8 {
    let result = connect(doors).and_then(|mut desktop| find(&mut desktop, name, spelling));
    let action = match result {
        Ok(action) => action,
        Err(failure) => return report(&failure, spelling, io),
    };
    let display = display_names(std::slice::from_ref(&action))
        .pop()
        .unwrap_or_default();
    let _ = writeln!(io.out, "{}", action.title);
    if let Some(subtitle) = &action.subtitle {
        let _ = writeln!(io.out, "{subtitle}");
    }
    let _ = writeln!(io.out);
    let _ = writeln!(io.out, "Name      {display}");
    if !action.kind.is_empty() {
        let _ = writeln!(io.out, "Kind      {}", action.kind);
    }
    let _ = writeln!(
        io.out,
        "Address   {}:{}",
        crate::client::printable_id(&action.provider),
        crate::client::printable_id(&action.id)
    );
    if action.params.is_empty() {
        let _ = writeln!(io.out, "Takes     nothing");
    } else {
        let _ = writeln!(io.out, "Takes");
        let width = action
            .params
            .iter()
            .map(|param| param.name.chars().count())
            .max()
            .unwrap_or(0);
        for param in &action.params {
            let need = if param.required {
                "required"
            } else {
                "optional"
            };
            let mut line = format!("  {:<width$}  {}, {need}", param.name, param.what());
            if !param.description.is_empty() && param.description != param.name {
                line.push_str("  ");
                line.push_str(&param.description);
            }
            let _ = writeln!(io.out, "{line}");
        }
    }
    let example = example_args(&action);
    let _ = writeln!(io.out, "\nExample\n  {} {display}{example}", spelling.run);
    0
}

fn example_args(action: &Action) -> String {
    action
        .params
        .iter()
        .filter(|param| param.required)
        .map(|param| {
            let value = param.choices.first().cloned().unwrap_or_else(|| {
                match param.ty.as_str() {
                    "integer" | "number" => "1",
                    "boolean" => "yes",
                    "array" => "'[]'",
                    "object" => "'{}'",
                    _ => "…",
                }
                .to_string()
            });
            (param.name.as_str(), value)
        })
        .fold(String::new(), |mut out, (name, value)| {
            use std::fmt::Write as _;
            let _ = write!(out, " {name}={value}");
            out
        })
}

/// `tilo do NAME [args]`: run one action, waiting for how it went.
pub fn run(
    doors: &[Door],
    name: &str,
    words: &[String],
    spelling: &Spelling,
    io: &mut Io<'_>,
) -> u8 {
    let mut desktop = match connect(doors) {
        Ok(desktop) => desktop,
        Err(failure) => return report(&failure, spelling, io),
    };
    let action = match find(&mut desktop, name, spelling) {
        Ok(action) => action,
        Err(failure) => return report(&failure, spelling, io),
    };
    let bound = if action.kind.is_empty() && action.params.is_empty() {
        // Named by its address: the desktop checks the arguments.
        bind_loose(words)
    } else {
        bind_args(&action, words)
    };
    let args = match bound {
        Ok(args) => args,
        Err(sentence) => return report(&Failure::Said(sentence), spelling, io),
    };
    invoke(
        &mut desktop,
        &action,
        &serde_json::Value::Object(args),
        spelling,
        io,
    )
}

fn invoke(
    desktop: &mut Desktop,
    action: &Action,
    args: &serde_json::Value,
    spelling: &Spelling,
    io: &mut Io<'_>,
) -> u8 {
    let request =
        crate::client::invoke_wire(&action.provider, &action.id, args, None, Some(RUN_WAIT_MS));
    let answer = match desktop.request(&request) {
        Ok(answer) => answer,
        Err(why) => return report(&Failure::Unreachable(why), spelling, io),
    };
    settle(&answer, action, spelling, io)
}

/// Say how an invocation went: its result, if it gave one; why it failed;
/// or that it is still going.
fn settle(answer: &serde_json::Value, action: &Action, spelling: &Spelling, io: &mut Io<'_>) -> u8 {
    if let Some(err) = answer.get("err") {
        let code = err
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if code == "invalid_arguments" {
            let message = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let _ = writeln!(io.err, "{message}");
            if !action.kind.is_empty() {
                let _ = writeln!(io.err, "It takes {}.", takes(action));
            }
            return 1;
        }
        return report(&Failure::Refused(err.clone()), spelling, io);
    }
    let answer = answer.get("answer").cloned().unwrap_or_default();
    let update = answer
        .pointer("/receipt/update")
        .cloned()
        .unwrap_or_default();
    let text = |pointer: &str| {
        update
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    match text("/state").as_deref() {
        Some("failed") => {
            let message = text("/error/message").unwrap_or_default();
            let _ = writeln!(
                io.err,
                "{}",
                if message.trim().is_empty() {
                    format!("“{}” didn't work.", action.title)
                } else {
                    message
                }
            );
            return 1;
        }
        Some("cancelled") => {
            let _ = writeln!(io.err, "“{}” was cancelled.", action.title);
            return 1;
        }
        Some("accepted" | "running" | "queued" | "pending") => {
            let _ = writeln!(io.err, "“{}” is still going.", action.title);
            return 0;
        }
        _ => {}
    }
    let value = update
        .pointer("/output/value")
        .or_else(|| answer.pointer("/receipt/output/value"))
        .cloned()
        .unwrap_or_default();
    match value {
        serde_json::Value::Null => {}
        serde_json::Value::String(text) => {
            let _ = writeln!(io.out, "{text}");
        }
        other => {
            let _ = writeln!(
                io.out,
                "{}",
                serde_json::to_string_pretty(&other).unwrap_or_else(|_| other.to_string())
            );
        }
    }
    0
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

/// The app `input` names among `actions`, as search and Enter would take
/// it: its id exactly, its name exactly, or the desktop's best match.
#[must_use]
pub fn pick_app(input: &str, actions: &[Action]) -> Option<usize> {
    let apps = || {
        actions
            .iter()
            .enumerate()
            .filter(|(_, action)| action.is_application())
    };
    let input = input.trim();
    let by_id = apps().find(|(_, action)| {
        let id = action.id.as_str();
        id == input || id.rsplit_once(':').is_some_and(|(_, tail)| tail == input)
    });
    let by_name = || apps().find(|(_, action)| action.title.eq_ignore_ascii_case(input));
    by_id
        .or_else(by_name)
        .or_else(|| apps().next())
        .map(|(at, _)| at)
}

/// The listing kinds that are apps: Tilo apps (here or to get) and the
/// host's own.
const APP_KINDS: &[&str] = &["app"];

/// The installed-app address that opens files and links (the shell's
/// `open-with:` row of the `applications` provider).
const OPEN_WITH_PREFIX: &str = "open-with:";

/// `tilo open APP [file|link…]`: open an app by id, name or a word of its
/// name, getting it first when this device does not have it yet, exactly
/// as search and Enter do; with files or links, open those in it.
pub fn open(
    doors: &[Door],
    app: &str,
    items: &[String],
    cwd: &Path,
    spelling: &Spelling,
    io: &mut Io<'_>,
) -> u8 {
    let mut desktop = match connect(doors) {
        Ok(desktop) => desktop,
        Err(failure) => return report(&failure, spelling, io),
    };
    let query = if app.contains('.') && !app.contains(' ') {
        app.to_string()
    } else {
        slug(app).replace('-', " ")
    };
    let listing = match listing(&mut desktop, &query, APP_KINDS) {
        Ok(listing) => listing,
        Err(failure) => return report(&failure, spelling, io),
    };
    let Some(chosen) = pick_app(app, &listing.actions)
        .and_then(|at| listing.actions.get(at))
        .cloned()
    else {
        let _ = writeln!(io.err, "There is no app called “{app}”.");
        return 1;
    };
    if items.is_empty() {
        return invoke(&mut desktop, &chosen, &serde_json::json!({}), spelling, io);
    }
    if chosen.provider != "applications" || chosen.id.contains(':') {
        let _ = writeln!(
            io.err,
            "{} isn't on this device yet. Open it once to get it: {} {}",
            chosen.title,
            spelling.open,
            slug(&chosen.title)
        );
        return 1;
    }
    let open_with = Action {
        id: format!("{OPEN_WITH_PREFIX}{}", chosen.id),
        params: Vec::new(),
        ..chosen
    };
    let items: Vec<String> = items.iter().map(|item| open_item(item, cwd)).collect();
    invoke(
        &mut desktop,
        &open_with,
        &serde_json::json!({ "open": items }),
        spelling,
        io,
    )
}

/// Names that complete `prefix` after `tilo do`: the desktop's own ranking
/// of the titles it holds, so a word inside a title completes too.
#[must_use]
pub fn complete_actions(doors: &[Door], prefix: &str) -> Vec<String> {
    let Ok(mut desktop) = Desktop::connect(doors) else {
        return Vec::new();
    };
    let query = slug(prefix).replace('-', " ");
    let Ok(listing) = listing(&mut desktop, &query, &[]) else {
        return Vec::new();
    };
    display_names(&listing.actions)
}

/// App names that complete `prefix` after `tilo open`.
#[must_use]
pub fn complete_apps(doors: &[Door], prefix: &str) -> Vec<String> {
    let Ok(mut desktop) = Desktop::connect(doors) else {
        return Vec::new();
    };
    let query = slug(prefix).replace('-', " ");
    let Ok(listing) = listing(&mut desktop, &query, APP_KINDS) else {
        return Vec::new();
    };
    let mut names: Vec<String> = listing
        .actions
        .iter()
        .filter(|action| action.is_application())
        .map(|action| slug(&action.title))
        .filter(|name| !name.is_empty())
        .collect();
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

#[cfg(test)]
#[path = "front_tests.rs"]
mod tests;
