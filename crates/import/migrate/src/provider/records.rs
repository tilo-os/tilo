//! The Migrations app's records of this device's migrations (`apps/tl/migrate`): what a review
//! found, what is chosen, how a run goes and what it brought, written as the app's own typed
//! actions through the migration's session, as any agent's call writes them. The window and
//! everyone else who opens the migration read them there; the engine keeps its journal here only
//! for itself.
//!
//! The writes are no step of a run: a run's History entry holds what it brought, and taking it
//! back leaves the migration's record to say so ([`Records::forgot`]).

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use crate::engine::services::created_id;
use crate::engine::state::{Journal, Place, Review, RunState};
use crate::engine::writer::{Invocation, Workspace};

/// The app whose records these are, as a call's path starts.
pub const APP: &str = "so.tilo.migrate";
/// Where a source keeps the id of its migration's record.
const RECORD: &str = "record";

/// The record of one source's migration on one device, reached through a session opened the first
/// time it is written.
pub struct Records<'a> {
    open: &'a dyn Fn() -> Result<Box<dyn Workspace>>,
    workspace: Option<Box<dyn Workspace>>,
    place: &'a Place,
    /// The key of the device that holds the data, hex, as the record names it.
    device: &'a str,
    source: String,
}

impl<'a> Records<'a> {
    /// The record of `source` on `device`, written through what `open` opens.
    pub fn of(
        open: &'a dyn Fn() -> Result<Box<dyn Workspace>>,
        place: &'a Place,
        device: &'a str,
        source: &str,
    ) -> Self {
        Self {
            open,
            workspace: None,
            place,
            device,
            source: source.to_owned(),
        }
    }

    /// Ask the app's `action` of the resource at `path` with `args`.
    fn ask(
        &mut self,
        path: &str,
        action: &str,
        args: Value,
    ) -> Result<crate::engine::writer::Answer> {
        if self.workspace.is_none() {
            self.workspace = Some((self.open)()?);
        }
        let workspace = self
            .workspace
            .as_mut()
            .ok_or_else(|| anyhow!("the migration's session did not open"))?;
        workspace
            .ask(&format!("{path}#{action}"), args)
            .with_context(|| format!("the Migrations app did not take {action}"))
    }

    /// Where the migration's record stands, as a call's path names it: made from the source's
    /// review the first time.
    fn record(&mut self) -> Result<String> {
        if let Some(id) = self.place.field::<String>(&self.source, RECORD)? {
            return Ok(path_of(&id));
        }
        let review = self
            .place
            .review(&self.source)?
            .ok_or_else(|| anyhow!("{} was not reviewed", self.source))?;
        self.reviewed(&review)
    }

    /// The review `review` stands: the migration's record says what it found, each item keeping
    /// whether a person chose it, and what it could not read. Its path.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn reviewed(&mut self, review: &Review) -> Result<String> {
        let made = self.ask(
            APP,
            "reviewed",
            json!({
                "source": review.source,
                "title": review.title,
                "device": self.device,
                "handle": review.handle,
            }),
        )?;
        let id = created_id(&made).ok_or_else(|| {
            anyhow!(
                "the Migrations app kept {} but did not say where",
                review.source
            )
        })?;
        self.place.keep_field(&self.source, RECORD, &id)?;
        let path = path_of(&id);
        for item in &review.items {
            self.ask(
                &path,
                "review_item",
                json!({
                    "key": item.key,
                    "kind": item.kind,
                    "title": item.title,
                    "count": item.count,
                    "counts": item.counts,
                    "within": item.within.as_deref().unwrap_or_default(),
                    "note": item.note.as_deref().unwrap_or_default(),
                    "chosen": item.chosen,
                }),
            )?;
        }
        let keep: Vec<&str> = review.items.iter().map(|item| item.key.as_str()).collect();
        self.ask(&path, "forget_items", json!({ "keep": keep }))?;
        for what in &review.unreadable {
            self.ask(&path, "unreadable", json!({ "what": what }))?;
        }
        Ok(path)
    }

    /// What of `review` is chosen now, exactly.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn chose(&mut self, review: &Review) -> Result<()> {
        let path = self.record()?;
        let keys: Vec<&str> = review
            .items
            .iter()
            .filter(|item| item.chosen)
            .map(|item| item.key.as_str())
            .collect();
        self.ask(
            &path,
            "choose_keys",
            json!({"keys": keys, "chosen": true, "only": true}),
        )
        .map(drop)
    }

    /// The run `run` began, or resumed.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn begun(&mut self, run: &Invocation) -> Result<()> {
        let path = self.record()?;
        self.ask(&path, "begun", json!({"run": run.id, "title": run.title}))
            .map(drop)
    }

    /// The run `run` brought `keys`, `count` things of `kind`, and found `already` there.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn landed(
        &mut self,
        run: &str,
        keys: &[String],
        kind: &str,
        count: usize,
        already: usize,
    ) -> Result<()> {
        let path = self.record()?;
        self.ask(
            &path,
            "landed",
            json!({"run": run, "keys": keys, "kind": kind, "count": count, "already": already}),
        )
        .map(drop)
    }

    /// The run ended as `journal` says: stopped, failed and why, or complete. A run still
    /// marked running ended without saying how, which is a failure.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn ended(&mut self, journal: &Journal) -> Result<()> {
        let path = self.record()?;
        let state = match journal.state {
            RunState::Reviewed | RunState::Running | RunState::Failed => RunState::Failed,
            RunState::Stopped => RunState::Stopped,
            RunState::Complete => RunState::Complete,
        };
        self.ask(
            &path,
            "ended",
            json!({
                "state": state,
                "failure": journal.failure.as_deref().unwrap_or_default(),
            }),
        )
        .map(drop)
    }

    /// The run `run` was taken back.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn forgot(&mut self, run: &str) -> Result<()> {
        let path = self.record()?;
        self.ask(&path, "forgot_run", json!({ "run": run }))
            .map(drop)
    }

    /// A step asked of the migration was refused, for `why`: the record says so until the next
    /// step goes through. A source never reviewed says it where it is offered.
    ///
    /// # Errors
    /// The app refused, or could not be reached.
    pub fn refused(&mut self, why: &str) -> Result<()> {
        if self.place.field::<String>(&self.source, RECORD)?.is_none()
            && self.place.review(&self.source)?.is_none()
        {
            // A source only offered says it on the migration the app lists for it, if any.
            let (source, device) = (self.source.clone(), self.device);
            return self
                .ask(
                    APP,
                    "source_refused",
                    json!({"source": source, "device": device, "why": why}),
                )
                .map(drop);
        }
        let path = self.record()?;
        self.ask(&path, "refused", json!({ "why": why })).map(drop)
    }
}

/// What the engine's `sources` answers, each source as the Migrations app offers it: its key and
/// title, why the device cannot read it, whether it is an account to connect, and what connecting
/// asks for. The device answers them; the app, which calls this action, puts them in its own
/// records, so a person on a device with no app reads the same list from `tilo migrate sources`.
///
/// # Errors
/// A source the engine lists and has no row for.
pub fn offered(device: &str, sources: &Value) -> Result<Value> {
    let listed = sources
        .get("sources")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("the engine listed no sources"))?;
    let mut offered = Vec::new();
    for one in listed {
        let Some(key) = one.get("source").and_then(Value::as_str) else {
            continue;
        };
        let row = crate::engine::registry::named(key)?;
        let asks = row.connect.unwrap_or_default();
        offered.push(json!({
            "source": row.key,
            "title": row.title,
            "unavailable": one.get("unavailable").and_then(Value::as_str).unwrap_or_default(),
            "here": one.get("here").cloned().unwrap_or(Value::Null),
            "next": one.get("next").cloned().unwrap_or(Value::Null),
            "connects": row.connect.is_some(),
            "asks_url": example_of(asks, "--url"),
            "asks_login": example_of(asks, "--login"),
        }));
    }
    Ok(json!({
        "device": device,
        "sources": offered,
        "reviewed": sources.get("reviewed").cloned().unwrap_or_else(|| json!([])),
    }))
}

/// The example a source's connect arguments give for `flag` (`--url https://cloud.example.com`
/// gives `https://cloud.example.com`), or empty when they name none.
fn example_of(arguments: &str, flag: &str) -> String {
    let mut words = arguments.split_whitespace();
    while let Some(word) = words.next() {
        if word == flag {
            return words.next().unwrap_or_default().to_owned();
        }
    }
    String::new()
}

/// The path of the Migrations app's record `id`. A record's id holds the steps of its document
/// and the item in it (`root-…/i…`): the address is one name in the path, so its slashes are
/// escaped.
fn path_of(id: &str) -> String {
    let address = if id.starts_with("tl:") {
        id.to_owned()
    } else {
        format!("tl:{APP}:{id}")
    };
    format!("{APP}/{}", address.replace('/', "\\/"))
}
