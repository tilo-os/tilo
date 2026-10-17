//! The project as Code Editor stores it, and the changes to it that are
//! more than editing one field.
//!
//! [`Project`] is the record: its files and folders in one [`Collection`],
//! each file's text in [`Project::buffers`] under the file's id, and the
//! embedded terminal's session. A file's text is a [`Source`]: collaborative
//! text read only while a view shows it, so a project holding every file it
//! ever opened costs what its open files weigh. Typing is a field edit at
//! `Project::paths().buffers.key(file)`; a file's language is a field edit
//! too. Creating, renaming, moving and deleting files and folders are the
//! verbs below, as is recording what the editor read from disk.

use std::collections::BTreeMap;

use automerge::{ObjId, ObjType, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use terminal::resource::{Finished, RunAnswer, RunRequest, Terminal};
use ui::tilo_resource::{
    self, ActionCx, Collection, Delta, Demand, Derived, Doc, EditOp, Id, Model, ModelError,
    Operations, Outbox, Refusal, SearchEntry, Segment, Slot, resource,
};

use crate::model::Language;

/// The longest text one file holds, in bytes.
pub const MAX_TEXT_BYTES: usize = crate::disk::MAX_TEXT_BYTES as usize;
/// The most a contextual edit carries: its text, what it deletes and its
/// context, in bytes.
pub const MAX_EDIT_BYTES: usize = 16 * 1024;
/// How much text on either side of a contextual edit identifies its place.
pub const CONTEXT_CHARS: usize = 32;

/// What an entry of the project is.
#[derive(
    Model,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    #[default]
    File,
    Folder,
}

impl EntryKind {
    /// The kind as the editor's tree names it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }
}

/// A file or folder of the project.
#[derive(Model, Default, Clone, Debug, PartialEq)]
#[model(check = Entry::valid)]
pub struct Entry {
    /// Kind
    #[model(readonly)]
    pub kind: EntryKind,
    /// Name
    ///
    /// Renamed with `rename`, which keeps names unique within a folder.
    #[model(label)]
    #[model(readonly)]
    #[model(atom)]
    pub name: String,
    /// Folder
    ///
    /// The folder it is in; none at the project's root. Changed with
    /// `move-entry`.
    #[model(readonly)]
    pub parent: Option<Id>,
    /// Language
    ///
    /// The language a file is highlighted and checked as, by its label
    /// (`Rust`, `Plain Text`); empty for a folder.
    #[model(atom)]
    pub language: String,
}

impl Entry {
    fn valid(&self) -> Result<(), Refusal> {
        valid_name(&self.name)?;
        if !self.language.is_empty() && crate::app::language_from_label(&self.language).is_none() {
            return Err(Refusal::new("the language is not one the editor knows"));
        }
        Ok(())
    }
}

/// A Code Editor project.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Project {
    /// Files
    ///
    /// The project's files and folders; a folder's entries in order.
    pub files: Collection<Entry>,
    /// Texts
    ///
    /// Each file's text, under the file's id, read while a view shows it.
    pub buffers: BTreeMap<String, Source>,
    /// Terminal
    ///
    /// The embedded terminal's session, recorded by the terminal running it.
    #[model(readonly)]
    pub terminal_panel: Option<Terminal>,
    /// Commands asked of the terminal
    ///
    /// The command lines asked to run in the embedded terminal's session,
    /// each settled with the run it became by the editor running it.
    #[model(readonly)]
    pub terminal_requests: Outbox<RunRequest, RunAnswer>,
}

/// The id a new file or folder is kept under.
fn minted(cx: &mut ActionCx) -> Id {
    Id::from(cx.uuid().to_string())
}

#[resource(
    kind = "code_editor.project",
    label = "Code Editor",
    search = Project::search_entries,
    children = Project::places,
    load = Project::load_texts
)]
impl Project {
    /// New file
    ///
    /// A new file named `name` in the folder `folder` (the project's root
    /// when none), holding `text`.
    #[action]
    pub fn new_file(
        &mut self,
        cx: &mut ActionCx,
        #[param("File name; `untitled` when none")] name: Option<String>,
        #[param("The folder it goes in; the project's root when none")] folder: Option<Id>,
        #[param("Its text")] text: Option<String>,
    ) -> Result<(), Refusal> {
        let name = untitled(name);
        let text = text.unwrap_or_default();
        if text.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new("the text is larger than a file holds"));
        }
        let id = minted(cx);
        self.add_entry(&id, EntryKind::File, name, folder)?;
        self.buffers.insert(id.to_string(), Source::new(text));
        Ok(())
    }

    /// New folder
    ///
    /// A new folder named `name` in the folder `folder` (the project's root
    /// when none).
    #[action]
    pub fn new_folder(
        &mut self,
        cx: &mut ActionCx,
        #[param("Folder name; `untitled` when none")] name: Option<String>,
        #[param("The folder it goes in; the project's root when none")] folder: Option<Id>,
    ) -> Result<(), Refusal> {
        let id = minted(cx);
        self.add_entry(&id, EntryKind::Folder, untitled(name), folder)
    }

    /// Rename
    ///
    /// Give the file or folder another name, unique in its folder.
    #[action(item = files)]
    pub fn rename(
        &mut self,
        entry: &Id,
        #[param("The new name")] name: String,
    ) -> Result<(), Refusal> {
        let parent = self.files.get(entry).ok_or("no such file")?.parent.clone();
        valid_name(&name)?;
        self.sibling_free(entry, parent.as_ref(), &name)?;
        if let Some(found) = self.files.get_mut(entry) {
            found.name = name;
        }
        Ok(())
    }

    /// Move
    ///
    /// Move the file or folder into the folder `folder` (the project's root
    /// when none), before `before` among its entries or after them all.
    #[action(item = files)]
    pub fn move_entry(
        &mut self,
        entry: &Id,
        #[param("The folder it goes in; the project's root when none")] folder: Option<Id>,
        #[param("The entry it goes before; after every entry when none")] before: Option<Id>,
    ) -> Result<(), Refusal> {
        let name = self.files.get(entry).ok_or("no such file")?.name.clone();
        if let Some(target) = &folder {
            self.folder(target)?;
            let mut cursor = Some(target.clone());
            while let Some(ancestor) = cursor {
                if &ancestor == entry {
                    return Err(Refusal::new("a folder cannot contain itself"));
                }
                cursor = self
                    .files
                    .get(&ancestor)
                    .and_then(|found| found.parent.clone());
            }
        }
        self.sibling_free(entry, folder.as_ref(), &name)?;
        let at = match &before {
            Some(before) if before != entry => self
                .files
                .position(before)
                .ok_or("the entry it goes before is gone")?,
            _ => self.files.len(),
        };
        if let Some(found) = self.files.get_mut(entry) {
            found.parent = folder;
        }
        let from = self.files.position(entry).unwrap_or_default();
        self.files
            .move_to(entry, if from < at { at - 1 } else { at });
        Ok(())
    }

    /// Replace text
    ///
    /// Replace the file's whole text with `text`, written as the smallest
    /// change to the text it has, so a collaborator's typing elsewhere in
    /// the file stays.
    #[action(item = files)]
    pub fn replace_text(
        &mut self,
        file: &Id,
        #[param("The whole new text")] text: String,
    ) -> Result<(), Refusal> {
        let entry = self.files.get(file).ok_or("no such file")?;
        if entry.kind != EntryKind::File {
            return Err(Refusal::new("a folder has no text"));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new("the text is larger than a file holds"));
        }
        self.buffers.insert(file.to_string(), Source::new(text));
        Ok(())
    }

    /// Replace range
    ///
    /// Replace a range of one file's text with `text`, or insert `text` at a
    /// caret, counting characters; the rest of the file stays as it is. A
    /// place a person selected is passed exactly as given.
    #[action]
    pub fn replace_range(
        &mut self,
        #[param(
            "Where: a range or a caret in one file, in characters",
            place = "files"
        )]
        at: tilo_resource::Place,
        #[param("The text that replaces the range")] text: String,
    ) -> Result<(), Refusal> {
        let (file, start, end) = match &at {
            tilo_resource::Place::Range { item, start, end } => {
                (item, (*start).min(*end), (*start).max(*end))
            }
            tilo_resource::Place::Caret { item, at } => (item, *at, *at),
            tilo_resource::Place::Item(_) => {
                return Err(Refusal::new("name a range or a caret in the file"));
            }
        };
        let entry = self
            .files
            .get(&Id::from(file.clone()))
            .ok_or("no such file")?;
        if entry.kind != EntryKind::File {
            return Err(Refusal::new("a folder has no text"));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new("the text is larger than a file holds"));
        }
        let source = self.buffers.get_mut(file).ok_or("the file has no text")?;
        match &mut source.initial {
            // A text not yet stored is edited where it is.
            Some(initial) => {
                *initial = splice_chars(initial, start, end, &text)
                    .ok_or("the range lies outside the file's text")?;
            }
            // A stored text is spliced in the document when the record is
            // stored, against the text it holds then, which this record may
            // not have read.
            None => source.splice = Some((start, end - start, text)),
        }
        Ok(())
    }

    /// Delete
    ///
    /// Delete the file and its text, or the folder once it is empty.
    #[action(item = files, destructive)]
    pub fn delete(&mut self, entry: &Id) -> Result<(), Refusal> {
        self.files.get(entry).ok_or("no such file")?;
        if self
            .files
            .values()
            .any(|found| found.parent.as_ref() == Some(entry))
        {
            return Err(Refusal::new("empty the folder before deleting it"));
        }
        self.files.remove(entry);
        self.buffers.remove(entry.as_str());
        Ok(())
    }

    /// Record entry
    ///
    /// Record a file or folder of the project's folder on disk, the first
    /// time the editor reads it, under the id the editor gave it. An entry
    /// already recorded, or at a path a collaborator recorded under another
    /// id, is left as it is.
    #[action(external)]
    pub fn record_entry(
        &mut self,
        #[param("The id it is kept under")] id: Id,
        #[param("File or folder")] kind: EntryKind,
        #[param("Its name")] name: String,
        #[param("The folder it is in; the project's root when none")] folder: Option<Id>,
        #[param("A file's language")] language: String,
    ) -> Result<(), Refusal> {
        if self.files.contains(&id) {
            return Ok(());
        }
        if let Some(folder) = &folder
            && self.folder(folder).is_err()
        {
            // The folder it is in was removed from the project.
            return Ok(());
        }
        if self.named(folder.as_ref(), &name).is_some() {
            return Ok(());
        }
        valid_id(&id)?;
        valid_name(&name)?;
        let at = self.files.len();
        self.files.insert(
            at,
            id,
            Entry {
                kind,
                name,
                parent: folder,
                language,
            },
        );
        Ok(())
    }

    /// Record text
    ///
    /// Keep `text`, read from disk, as the text of `file` the first time
    /// the editor opens it. Text already there — a collaborator's or an
    /// earlier session's unsaved edits — is left as it is.
    #[action(external)]
    pub fn record_text(
        &mut self,
        #[param("The file's id")] file: String,
        #[param("Its text on disk")] text: String,
    ) -> Result<(), Refusal> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new("the text is larger than a file holds"));
        }
        self.buffers
            .entry(file)
            .or_insert_with(|| Source::from_disk(text));
        Ok(())
    }

    /// Record terminal target
    ///
    /// Record where the embedded terminal's session runs.
    #[action(external, id = "record-target")]
    pub fn record_terminal_target(
        &mut self,
        #[param("An encoded session target")] target: String,
    ) -> Result<(), Refusal> {
        self.terminal_panel
            .get_or_insert_with(Terminal::default)
            .record_target(target)
    }

    /// Record terminal relay session
    ///
    /// Record the relay session the embedded terminal re-attaches to, or
    /// forget it.
    #[action(external, id = "record-relay-session")]
    pub fn record_terminal_relay(
        &mut self,
        #[param("The relay session's id; none to forget it")] session: Option<String>,
    ) -> Result<(), Refusal> {
        self.terminal_panel
            .get_or_insert_with(Terminal::default)
            .record_relay_session(session)
    }

    /// Claim terminal pane
    ///
    /// Scope presence to a new process's pane of the embedded terminal.
    #[action(external, id = "claim-pane")]
    pub fn claim_terminal_pane(
        &mut self,
        #[param("The pane's id")] pane: String,
    ) -> Result<(), Refusal> {
        self.terminal_panel
            .get_or_insert_with(Terminal::default)
            .claim_pane(pane)
    }

    /// Run in terminal
    ///
    /// Run a command line in the embedded terminal's session, and answer
    /// with the run once it ends.
    #[action(id = "run")]
    pub fn run_in_terminal(
        &mut self,
        cx: &mut ActionCx,
        terminal_requests: &mut Outbox<RunRequest, RunAnswer>,
        #[param("The command line, as typed at the prompt")] command: String,
    ) -> Result<(), Refusal> {
        self.terminal()?.run(cx, terminal_requests, command)
    }

    /// Rerun in terminal
    ///
    /// Run one of the embedded terminal's runs again.
    #[action(id = "rerun")]
    pub fn rerun_in_terminal(
        &mut self,
        cx: &mut ActionCx,
        terminal_requests: &mut Outbox<RunRequest, RunAnswer>,
        #[param("The run")] run: Id,
    ) -> Result<(), Refusal> {
        self.terminal()?.rerun(cx, terminal_requests, &run)
    }

    /// Rerun test in terminal
    ///
    /// Run one test of one of the embedded terminal's runs again, the way
    /// its tool runs one test.
    #[action(id = "rerun-test")]
    pub fn rerun_test_in_terminal(
        &mut self,
        cx: &mut ActionCx,
        terminal_requests: &mut Outbox<RunRequest, RunAnswer>,
        #[param("The run")] run: Id,
        #[param("The test's name, as the run reported it")] test: String,
    ) -> Result<(), Refusal> {
        self.terminal()?
            .rerun_test(cx, terminal_requests, &run, test)
    }

    /// Record terminal run
    ///
    /// Record a run the embedded terminal's session ended.
    #[action(external, id = "record-run")]
    pub fn record_terminal_run(
        &mut self,
        cx: &mut ActionCx,
        #[param("The run, as the session marked it", flatten)] run: Finished,
    ) -> Result<(), Refusal> {
        self.terminal_panel
            .get_or_insert_with(Terminal::default)
            .record_run(cx, run)
    }
}

impl Project {
    /// The embedded terminal's session, once it has one.
    fn terminal(&mut self) -> Result<&mut Terminal, Refusal> {
        self.terminal_panel
            .as_mut()
            .ok_or_else(|| Refusal::new("the project's terminal has not run yet"))
    }

    /// The entry `id` when it is a folder.
    fn folder(&self, id: &Id) -> Result<&Entry, Refusal> {
        self.files
            .get(id)
            .filter(|found| found.kind == EntryKind::Folder)
            .ok_or_else(|| Refusal::new("the folder is not a folder of the project"))
    }

    /// The entry named `name` (ignoring case) in `folder`.
    fn named(&self, folder: Option<&Id>, name: &str) -> Option<&Id> {
        self.files
            .iter()
            .find(|(_, found)| {
                found.parent.as_ref() == folder && found.name.eq_ignore_ascii_case(name)
            })
            .map(|(id, _)| id)
    }

    fn sibling_free(&self, entry: &Id, folder: Option<&Id>, name: &str) -> Result<(), Refusal> {
        match self.named(folder, name) {
            Some(other) if other != entry => Err(Refusal::new(
                "an entry of that name is in the folder already",
            )),
            _ => Ok(()),
        }
    }

    fn add_entry(
        &mut self,
        id: &Id,
        kind: EntryKind,
        name: String,
        folder: Option<Id>,
    ) -> Result<(), Refusal> {
        valid_name(&name)?;
        if let Some(folder) = &folder {
            self.folder(folder)?;
        }
        self.sibling_free(id, folder.as_ref(), &name)?;
        let language = match kind {
            EntryKind::File => Language::from_path(&name).label().to_owned(),
            EntryKind::Folder => String::new(),
        };
        let at = self.files.len();
        self.files.insert(
            at,
            id.clone(),
            Entry {
                kind,
                name,
                parent: folder,
                language,
            },
        );
        Ok(())
    }

    /// The path of entry `id` in the project, its folders joined by `/`.
    #[must_use]
    pub fn path_of(&self, id: &Id) -> Option<String> {
        let mut parts = Vec::new();
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            let found = self.files.get(current)?;
            parts.push(found.name.clone());
            if parts.len() > self.files.len() {
                return None;
            }
            cursor = found.parent.as_ref();
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// How many texts are read now, and their size in bytes.
    #[must_use]
    pub fn read_texts(&self) -> (usize, usize) {
        self.buffers
            .values()
            .filter_map(|source| source.text.as_deref())
            .fold((0, 0), |(count, bytes), text| {
                (count + 1, bytes + text.len())
            })
    }

    /// The text of file `id`, when a view asked for it.
    #[must_use]
    pub fn text(&self, id: &str) -> Option<&str> {
        self.buffers.get(id).and_then(Source::text)
    }

    /// Read the texts of the files the views show — the demand's keys are
    /// their ids — and let go of the others.
    ///
    /// # Errors
    /// Returns [`ModelError`] when a text is no longer a text.
    pub fn load_texts(&mut self, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        for (id, source) in &mut self.buffers {
            if demand.keys.contains(id) {
                source.read(doc)?;
            } else {
                source.release();
            }
        }
        Ok(())
    }

    /// Every file and folder by its name and path, each opening it.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.files
            .iter()
            .map(|(id, entry)| {
                SearchEntry::new(
                    entry.name.clone(),
                    self.path_of(id).unwrap_or_else(|| entry.name.clone()),
                )
                .anchored(crate::actions::file_resource_id(id))
            })
            .collect()
    }

    /// The embedded terminal, once the project has one.
    fn places(&self) -> Vec<Derived<Self>> {
        self.terminal_panel
            .iter()
            .map(|_| {
                Derived::new("Terminal")
                    .identified("terminal")
                    .of_kind("terminal")
            })
            .collect()
    }
}

fn untitled(name: Option<String>) -> String {
    name.filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "untitled".to_owned())
}

fn valid_id(id: &Id) -> Result<(), Refusal> {
    if uuid::Uuid::parse_str(id.as_str()).is_err() {
        return Err(Refusal::new("an entry's id is a UUID"));
    }
    Ok(())
}

fn valid_name(name: &str) -> Result<(), Refusal> {
    if name.trim().is_empty()
        || name.len() > 255
        || name.contains(['/', '\\'])
        || matches!(name, "." | "..")
    {
        return Err(Refusal::new("the name is not a file name"));
    }
    Ok(())
}

/// `text` with characters `start..end` replaced by `insert`, or `None` when
/// the range runs past its end.
fn splice_chars(text: &str, start: usize, end: usize, insert: &str) -> Option<String> {
    let byte = |chars: usize| {
        text.char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .nth(chars)
    };
    let (from, to) = (byte(start)?, byte(end)?);
    let mut next = String::with_capacity(text.len() - (to - from) + insert.len());
    next.push_str(text.get(..from)?);
    next.push_str(insert);
    next.push_str(text.get(to..)?);
    Some(next)
}

/// The SHA-256 of `text`, as a contextual edit names the text it was made
/// against.
#[must_use]
pub fn text_digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// A file's text: collaborative text, read only while a view shows it.
///
/// Read from a document it holds no text until [`Project::load_texts`]
/// reads it; a new value ([`Source::new`]) writes its text when the record
/// is stored. A value read from a document never writes back.
#[derive(Clone, Debug, Default)]
pub struct Source {
    object: Option<ObjId>,
    /// Shared, so a copy of the project (a view holding the state it was
    /// shown while the reader follows) copies no file's text.
    text: Option<std::sync::Arc<str>>,
    initial: Option<String>,
    /// Whether a new text is the same object on every device that makes it.
    shared: bool,
    /// A splice (characters at, deleted, inserted) written into the stored
    /// text when the record is stored.
    splice: Option<(usize, usize, String)>,
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        self.object == other.object
            && self.text == other.text
            && self.initial == other.initial
            && self.shared == other.shared
            && self.splice == other.splice
    }
}

impl Source {
    /// A new text holding `text`, written when the record is stored.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            object: None,
            text: None,
            initial: Some(text.into()),
            shared: false,
            splice: None,
        }
    }

    /// A new text holding `text` as a file on disk holds it: every device
    /// that reads the same file into the same record makes the same text
    /// object, so two editors opening it before they sync type into one
    /// text, not two competing ones.
    #[must_use]
    pub fn from_disk(text: impl Into<String>) -> Self {
        Self {
            shared: true,
            ..Self::new(text)
        }
    }

    /// The text, when it was read.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref().or(self.initial.as_deref())
    }

    /// The text object it is kept in, once stored.
    #[must_use]
    pub fn object(&self) -> Option<&ObjId> {
        self.object.as_ref()
    }

    fn read(&mut self, doc: &Doc) -> Result<(), ModelError> {
        if self.text.is_none()
            && let Some(object) = &self.object
        {
            self.text = Some(doc.text(object)?.into());
        }
        Ok(())
    }

    fn release(&mut self) {
        self.text = None;
    }
}

/// An edit of a file's text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum SourceEdit {
    /// Replace `delete` characters at `at` with `insert`. Positions count
    /// Unicode scalar values.
    Splice {
        at: usize,
        delete: usize,
        insert: String,
    },
    /// Replace `delete_chars` characters at `start_chars` with `insert`, made
    /// against the text whose SHA-256 is `expected_base_digest`. When the
    /// text changed since, the edit lands where `before_context`,
    /// `deleted_text` and `after_context` (up to 32 characters each side)
    /// still read as one untouched place, and is refused otherwise.
    Contextual {
        expected_base_digest: String,
        start_chars: usize,
        delete_chars: usize,
        insert: String,
        #[serde(default)]
        before_context: String,
        #[serde(default)]
        deleted_text: String,
        #[serde(default)]
        after_context: String,
    },
}

impl SourceEdit {
    /// The one smallest contextual splice turning `base` into `next`.
    #[must_use]
    pub fn between(base: &str, next: &str) -> Self {
        let base_chars: Vec<char> = base.chars().collect();
        let next_chars: Vec<char> = next.chars().collect();
        let prefix = base_chars
            .iter()
            .zip(&next_chars)
            .take_while(|(left, right)| left == right)
            .count();
        let base_tail = base_chars.get(prefix..).unwrap_or_default();
        let next_tail = next_chars.get(prefix..).unwrap_or_default();
        let suffix = base_tail
            .iter()
            .rev()
            .zip(next_tail.iter().rev())
            .take_while(|(left, right)| left == right)
            .count();
        let deleted: String = base_tail
            .iter()
            .take(base_tail.len().saturating_sub(suffix))
            .collect();
        let end = prefix + deleted.chars().count();
        Self::Contextual {
            expected_base_digest: text_digest(base),
            start_chars: prefix,
            delete_chars: deleted.chars().count(),
            insert: next_tail
                .iter()
                .take(next_tail.len().saturating_sub(suffix))
                .collect(),
            before_context: base_chars
                .iter()
                .skip(prefix.saturating_sub(CONTEXT_CHARS))
                .take(prefix.min(CONTEXT_CHARS))
                .collect(),
            deleted_text: deleted,
            after_context: base_chars.iter().skip(end).take(CONTEXT_CHARS).collect(),
        }
    }

    /// The plain splice this edit is against `text`, or why it no longer
    /// fits.
    fn splice(&self, text: &str) -> Result<(usize, usize, String), String> {
        match self {
            Self::Splice { at, delete, insert } => Ok((*at, *delete, insert.clone())),
            Self::Contextual {
                expected_base_digest,
                start_chars,
                delete_chars,
                insert,
                before_context,
                deleted_text,
                after_context,
            } => {
                let carried = insert
                    .len()
                    .saturating_add(deleted_text.len())
                    .saturating_add(before_context.len())
                    .saturating_add(after_context.len());
                if carried > MAX_EDIT_BYTES
                    || deleted_text.chars().count() != *delete_chars
                    || before_context.chars().count() > CONTEXT_CHARS
                    || after_context.chars().count() > CONTEXT_CHARS
                {
                    return Err("the edit exceeds its bounds or deletion count".into());
                }
                let length = text.chars().count();
                if text_digest(text) == *expected_base_digest
                    && *start_chars <= length
                    && *delete_chars <= length - start_chars
                {
                    return Ok((*start_chars, *delete_chars, insert.clone()));
                }
                rebase(text, before_context, deleted_text, after_context)
                    .map(|(at, delete)| (at, delete, insert.clone()))
                    .ok_or_else(|| "the text changed where the edit was made".into())
            }
        }
    }
}

/// Where `before + deleted + after` reads exactly once in `text`: the place
/// an edit made against an older text still fits.
fn rebase(text: &str, before: &str, deleted: &str, after: &str) -> Option<(usize, usize)> {
    let needle = format!("{before}{deleted}{after}");
    if needle.is_empty() {
        return None;
    }
    let mut matches = text.match_indices(&needle);
    let (byte, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let start = text.get(..byte + before.len())?.chars().count();
    Some((start, deleted.chars().count()))
}

impl Operations for Source {
    type Op = SourceEdit;

    /// Typing a word is one undo step: an insertion right where the last
    /// one ended goes on until a space ends the word; deleting runs the same
    /// way, backwards or forwards.
    fn coalesces(previous: &SourceEdit, next: &SourceEdit) -> bool {
        let ((before_at, before_delete, before_insert), (at, delete, insert)) =
            (plain(previous), plain(next));
        match (
            before_delete,
            delete,
            before_insert.is_empty(),
            insert.is_empty(),
        ) {
            (0, 0, false, false) => {
                at == before_at.saturating_add(before_insert.chars().count())
                    && !before_insert.ends_with(char::is_whitespace)
            }
            (1.., 1.., true, true) => at.saturating_add(delete) == before_at || at == before_at,
            _ => false,
        }
    }

    /// What history says of an edit: text typed or deleted.
    fn describe(_: &SourceEdit) -> tilo_resource::path::OpDescription {
        tilo_resource::path::OpDescription {
            op: ui::tilo_actions::EditOp::Splice,
            args: std::collections::BTreeMap::new(),
        }
    }
}

/// An edit's place, size and text, as written.
fn plain(edit: &SourceEdit) -> (usize, usize, &str) {
    match edit {
        SourceEdit::Splice { at, delete, insert } => (*at, *delete, insert),
        SourceEdit::Contextual {
            start_chars,
            delete_chars,
            insert,
            ..
        } => (*start_chars, *delete_chars, insert),
    }
}

fn refused(error: ui::tilo_actions::ActionError) -> ModelError {
    match error {
        ui::tilo_actions::ActionError::Refused(reason) => ModelError::Refused(reason),
        other => ModelError::invalid(other.to_string()),
    }
}

impl Model for Source {
    tilo_resource::leaf_paths!();
    tilo_resource::operations_model!();

    fn to_json(&self) -> serde_json::Value {
        self.text().map_or(serde_json::Value::Null, |text| {
            serde_json::Value::String(text.to_owned())
        })
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => Ok(Self::new(text.clone())),
            serde_json::Value::Null => Ok(Self::new(String::new())),
            _ => Err(ModelError::shape("text")),
        }
    }

    fn load(_: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Text)) {
            return Err(ModelError::shape("text"));
        }
        Ok(Self {
            object: Some(id.clone()),
            text: None,
            initial: None,
            shared: false,
            splice: None,
        })
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let Some(text) = &self.initial else {
            let object = slot.object(doc, ObjType::Text)?;
            if let Some((at, delete, insert)) = &self.splice {
                let current = doc.text(&object)?;
                let length = current.chars().count();
                if at.checked_add(*delete).is_none_or(|end| end > length) {
                    return Err(ModelError::Refused(
                        "the range lies outside the file's text".into(),
                    ));
                }
                let removed: usize = current
                    .chars()
                    .skip(*at)
                    .take(*delete)
                    .map(char::len_utf8)
                    .sum();
                if current.len() - removed + insert.len() > MAX_TEXT_BYTES {
                    return Err(ModelError::Refused(format!(
                        "the edit would make the file larger than {MAX_TEXT_BYTES} bytes"
                    )));
                }
                tilo_resource::TextEdit::Splice {
                    at: *at,
                    delete: *delete,
                    insert: insert.clone(),
                }
                .apply_under(doc, &object)
                .map_err(refused)?;
            }
            return Ok(());
        };
        let shared = match (slot.current(doc)?, slot) {
            (None, Slot::Key(parent, key)) if self.shared => {
                ui::record::bootstrap::object(doc, parent, key, ObjType::Text)
            }
            _ => None,
        };
        if let Some(object) = &shared
            && ui::record::bootstrap::seed_text(doc, object, text).is_some()
        {
            return Ok(());
        }
        let object = match shared {
            Some(object) => object,
            None => slot.object(doc, ObjType::Text)?,
        };
        tilo_resource::TextEdit::ReplaceText { text: text.clone() }
            .apply_under(doc, &object)
            .map_err(refused)
    }

    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        _delta: &Delta,
    ) -> Result<(), ModelError> {
        let shown = self.text.is_some();
        *self = Self::load(doc, value, id)?;
        if shown {
            self.read(doc)?;
        }
        Ok(())
    }

    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        if let Some(segment) = path.first() {
            return Err(ModelError::invalid(format!(
                "nothing inside a file's text is called `{segment}`"
            )));
        }
        let Some((Value::Object(ObjType::Text), object)) = slot.current(doc)? else {
            return Err(ModelError::Refused(
                "the file has no text here to edit".into(),
            ));
        };
        let text = doc.text(&object)?;
        let (at, delete, insert) = match op {
            EditOp::Rich { rich } => serde_json::from_value::<SourceEdit>(rich.clone())
                .map_err(|error| ModelError::invalid(error.to_string()))?
                .splice(&text)
                .map_err(ModelError::Refused)?,
            EditOp::Splice { at, delete, insert } => (*at, *delete, insert.clone()),
            EditOp::Replace { text: next } => {
                match tilo_resource::text::splice_between(&text, next) {
                    Some(tilo_resource::TextEdit::Splice { at, delete, insert }) => {
                        (at, delete, insert)
                    }
                    _ => return Ok(()),
                }
            }
            other => return Err(ModelError::unsupported(other)),
        };
        let removed: usize = text.chars().skip(at).take(delete).map(char::len_utf8).sum();
        if text
            .len()
            .saturating_sub(removed)
            .saturating_add(insert.len())
            > MAX_TEXT_BYTES
        {
            return Err(ModelError::Refused(format!(
                "the edit would make the file larger than {MAX_TEXT_BYTES} bytes"
            )));
        }
        tilo_resource::TextEdit::Splice { at, delete, insert }
            .apply_under(doc, &object)
            .map_err(refused)
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        if let Some(text) = self.text() {
            out.push(text.to_owned());
        }
    }
}

#[cfg(test)]
#[path = "project/tests.rs"]
mod tests;
