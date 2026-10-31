//! `org.freedesktop.impl.portal.FileChooser`.
//!
//! Three methods, same shape: `OpenFile`/`SaveFile`/`SaveFiles(handle,
//! app_id, parent_window, title, options) → (response, results)`, with
//! `uris` mandatory in the results on success.
//!
//! Each call is asked as a question of kind [`FILE_CHOOSER_QUESTION`]
//! ([`FileChooserQuestion`]); the person's answer names the files (or the
//! place) by URI, and those become the reply. **The chooser is the
//! prompt**: there is deliberately no separate "may this app read your
//! files?" question in front of it, since choosing a file is the act of
//! authorising exactly that file, nothing wider and nothing the app
//! chose. What each answer names comes from [`crate::doc_store`]: a
//! sandboxed app is given its own path in the session's documents store,
//! which its launcher bound into it, and a host app the path itself, so
//! nothing is exported by the frontend and nothing is recorded in any
//! workspace.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tilo_service::Question;
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::doc_store::{self, Export};
use crate::reply::{Ended, OrFail, Outcome, Reply, Results, answer};
use crate::{Portal, PortalCaller, lockdown, option, question_handle};

/// The kind of the question a file chooser call asks.
pub const FILE_CHOOSER_QUESTION: &str = "portal.file-chooser";

/// One named file-type filter the calling app asked for.
///
/// Decoded from the portal's `filters` option (`a(sa(us))`): a list of
/// `(label, [(kind, pattern)])` where kind 0 is a glob, kind 1 a MIME
/// type. Both are kept: an app filtering by MIME (`image/png`) and one
/// filtering by glob (`*.png`) are asking the same question.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerFilter {
    /// What the person sees in the filter control ("Images"), in the
    /// app's words.
    pub label: String,
    /// Lower-cased extensions without the dot, derived from globs.
    pub extensions: Vec<String>,
    /// MIME types named directly by the app.
    pub mimetypes: Vec<String>,
}

impl PickerFilter {
    /// Build from a label plus the raw `(kind, pattern)` pairs.
    ///
    /// A glob is reduced to its extension because that's what a chooser
    /// can match cheaply against a directory listing; a pattern with no
    /// usable extension (`*`, `*.*`, `report-??.txt`) widens the filter to
    /// everything rather than silently matching nothing: hiding files the
    /// app would have accepted is worse than showing extra ones.
    #[must_use]
    pub fn from_pairs(label: impl Into<String>, pairs: &[(u32, String)]) -> Self {
        let mut extensions = Vec::new();
        let mut mimetypes = Vec::new();
        let mut matches_everything = false;
        for (kind, pattern) in pairs {
            match kind {
                1 => mimetypes.push(pattern.to_ascii_lowercase()),
                _ => match extension_of_glob(pattern) {
                    Some(ext) => extensions.push(ext),
                    None => matches_everything = true,
                },
            }
        }
        if matches_everything {
            extensions.clear();
            mimetypes.clear();
        }
        Self {
            label: label.into(),
            extensions,
            mimetypes,
        }
    }

    /// `true` when this filter accepts every file.
    #[must_use]
    pub fn is_catch_all(&self) -> bool {
        self.extensions.is_empty() && self.mimetypes.is_empty()
    }
}

/// Reduce a shell glob to the extension it selects, if it selects one.
///
/// `*.png` → `png`. `*`/`*.*` select everything, and a glob whose
/// wildcards fall inside the extension (`*.p?g`) can't be reduced: both
/// answer `None`, read as "matches everything".
fn extension_of_glob(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("*.")?;
    if rest.is_empty() || rest.contains(['*', '?', '[', ']', '.']) {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

/// What the chooser shows for one call.
///
/// Note what is *not* here: a starting folder the app chose. The portal's
/// `current_folder` option is deliberately dropped: an app that could aim
/// the chooser could use it to probe for files it was never given, by
/// reading which folders the chooser opens. Where it starts is the
/// session's decision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one of the portal's independent options"
)]
pub struct FileChooserQuestion {
    /// The window the app says it asks from (`wayland:<handle>` or
    /// `x11:<id>`), when it named one: the chooser is a sheet for that
    /// window, picked among the caller's own.
    pub parent_window: Option<String>,
    /// The title the app gave.
    pub title: String,
    /// A place to save to rather than files to open.
    pub save: bool,
    /// Folders rather than files.
    pub directory: bool,
    /// The app may write into the chosen folder: the chooser words this
    /// itself, never in the app's words, so an app cannot disguise it as
    /// an ordinary Open.
    pub writable_directory: bool,
    /// More than one may be chosen.
    pub multiple: bool,
    /// The app's preferred label for the confirm button ("Import").
    pub accept_label: Option<String>,
    /// The name a saved file starts with.
    pub current_name: Option<String>,
    /// The file-type filters the app offered; none means any file.
    pub filters: Vec<PickerFilter>,
}

/// The `org.freedesktop.impl.portal.FileChooser` interface.
pub struct FileChooser {
    portal: Portal,
}

impl FileChooser {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    async fn choose(
        &self,
        header: &zbus::message::Header<'_>,
        connection: &zbus::Connection,
        handle: &ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        question: FileChooserQuestion,
    ) -> Outcome {
        let auth = self
            .portal
            .authenticate(header, handle, app_id, Some(parent_window))
            .await?;
        if (question.save || question.writable_directory)
            && lockdown::current_policy().disable_save_to_disk
        {
            return Err(Ended::Cancelled);
        }
        let path = handle.as_str();
        self.portal
            .requests
            .run(
                connection,
                path,
                auth.owner.as_str(),
                pick(&self.portal, &auth.caller, path, question),
            )
            .await
    }
}

/// Ask `question` for `caller` as request `path`, and reply with what the
/// person chose.
///
/// `writable_directory` on a directory is explicit destination consent:
/// the frontend exports only that chosen folder with write permission.
/// Ordinary Open requests are read-only; Save requests write.
pub async fn pick(
    portal: &Portal,
    caller: &PortalCaller,
    path: &str,
    question: FileChooserQuestion,
) -> Outcome {
    let writable_directory = question.directory && question.writable_directory;
    // What the choice gives the app, and how the frontend words the answer:
    // one source, so an answer can never widen what the store was told.
    let kind = if question.save {
        tilo_documents::Kind::Save
    } else if question.directory {
        tilo_documents::Kind::Folder {
            write: writable_directory,
        }
    } else {
        tilo_documents::Kind::Read
    };
    let writable = kind.writes();
    let asked = portal.questions.ask(Question {
        handle: question_handle(path),
        kind: FILE_CHOOSER_QUESTION.into(),
        app_id: Some(caller.app_id.clone()),
        label: caller.app_id.clone(),
        details: serde_json::to_value(&question).unwrap_or_default(),
    });
    let chosen = asked.await.ok_or(Ended::Cancelled)?;
    let paths = chosen
        .uris
        .iter()
        .map(|uri| path_of_uri(uri))
        .collect::<Option<Vec<_>>>()
        .or_fail()?;
    if paths.len() > 1 && !question.multiple && !question.save {
        return Err(Ended::Failed);
    }
    if writable_directory && (paths.len() != 1 || !paths.iter().all(|path| path.is_dir())) {
        return Err(Ended::Failed);
    }
    let exports = doc_store::exports(portal.documents(), caller, &paths, kind);
    let mut results = paths_to_open_response(Some(exports), writable)?;
    if writable_directory {
        let choices = vec![(
            tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE.to_owned(),
            "true".to_owned(),
        )];
        if let Ok(value) = OwnedValue::try_from(Value::new(choices)) {
            results.insert("choices".into(), value);
        }
    }
    Ok(results)
}

/// The local path a chosen `file://` URI names; `None` for anything else.
fn path_of_uri(uri: &str) -> Option<PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    (url.scheme() == "file")
        .then(|| url.to_file_path().ok())
        .flatten()
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        4
    }

    /// `OpenFile`: one or more existing files, or folders.
    async fn open_file(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        let question = decode_open_options(title, &options).parent_window(&parent_window);
        answer(self.choose(
            &header,
            connection,
            &handle,
            &app_id,
            &parent_window,
            question,
        ))
        .await
    }

    /// `SaveFile`: a destination for one new file.
    async fn save_file(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        let question = FileChooserQuestion {
            title,
            save: true,
            accept_label: opt_string(&options, "accept_label"),
            current_name: opt_string(&options, "current_name"),
            filters: filters_from_options(&options),
            ..FileChooserQuestion::default()
        }
        .parent_window(&parent_window);
        answer(self.choose(
            &header,
            connection,
            &handle,
            &app_id,
            &parent_window,
            question,
        ))
        .await
    }

    /// `SaveFiles`: a folder to save several files into.
    async fn save_files(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        let question = FileChooserQuestion {
            title,
            save: true,
            accept_label: opt_string(&options, "accept_label"),
            filters: filters_from_options(&options),
            ..FileChooserQuestion::default()
        }
        .parent_window(&parent_window);
        answer(self.choose(
            &header,
            connection,
            &handle,
            &app_id,
            &parent_window,
            question,
        ))
        .await
    }
}

impl FileChooserQuestion {
    /// The portal's `parent_window` argument; an empty handle names none.
    #[must_use]
    pub fn parent_window(mut self, handle: &str) -> Self {
        self.parent_window = Some(handle.trim())
            .filter(|handle| !handle.is_empty())
            .map(str::to_string);
        self
    }
}

/// Decode the portal's `filters` option: `a(sa(us))`.
///
/// A malformed entry is skipped rather than failing the call: filters are
/// a display convenience, and refusing to open a chooser over one bad
/// filter would turn a cosmetic problem into an unusable Open menu.
fn filters_from_options(options: &HashMap<String, OwnedValue>) -> Vec<PickerFilter> {
    let Some(value) = options.get("filters").and_then(|v| v.try_clone().ok()) else {
        return Vec::new();
    };
    let Ok(entries) = <Vec<(String, Vec<(u32, String)>)>>::try_from(value) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|(label, pairs)| PickerFilter::from_pairs(label, &pairs))
        .collect()
}

fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let v = options.get(key)?.try_clone().ok()?;
    String::try_from(v).ok().filter(|s| !s.is_empty())
}

/// Only a single-directory request can opt into writing. Unknown or
/// malformed choices and ordinary Open requests stay read-only.
fn writable_directory_requested(options: &HashMap<String, OwnedValue>) -> bool {
    type Choices = Vec<(String, String, Vec<(String, String)>, String)>;
    if option::<bool>(options, "directory") != Some(true)
        || option::<bool>(options, "multiple").unwrap_or(false)
    {
        return false;
    }
    options
        .get("choices")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Choices::try_from(value).ok())
        .is_some_and(|choices| {
            choices.iter().any(|(id, _, alternatives, selected)| {
                id == tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE
                    && alternatives.is_empty()
                    && selected == "true"
            })
        })
}

fn decode_open_options(
    title: String,
    options: &HashMap<String, OwnedValue>,
) -> FileChooserQuestion {
    FileChooserQuestion {
        title,
        directory: option::<bool>(options, "directory").unwrap_or(false),
        multiple: option::<bool>(options, "multiple").unwrap_or(false),
        writable_directory: writable_directory_requested(options),
        accept_label: opt_string(options, "accept_label"),
        filters: filters_from_options(options),
        ..FileChooserQuestion::default()
    }
}

/// Translate what the person chose into the results the `FileChooser`
/// frontend expects. No selection (`None`, or nothing chosen) is a cancel.
///
/// Each answer names what the app opens ([`Export::app_path`]): the store
/// path of a sandboxed app, or the host path itself. `writable` is what the
/// choice gives the app.
///
/// # Errors
/// [`Ended::Cancelled`] for no selection, [`Ended::Failed`] for a path no
/// URI can name.
pub fn paths_to_open_response(exports: Option<Vec<Export>>, writable: bool) -> Outcome {
    let mut results = Results::new();
    let exports = exports
        .filter(|exports| !exports.is_empty())
        .ok_or(Ended::Cancelled)?;
    let uris = exports
        .iter()
        .map(|export| file_uri(&export.app_path))
        .collect::<Option<Vec<_>>>()
        .or_fail()?;
    if let Ok(owned) = OwnedValue::try_from(Value::new(uris)) {
        results.insert("uris".to_string(), owned);
    }
    // The frontend defaults a missing `writable` to true. Always state the
    // access explicitly so a read-only Open cannot acquire write permission
    // on what the app was handed.
    results.insert("writable".to_string(), OwnedValue::from(writable));
    Ok(results)
}

/// Encode an absolute path as an RFC 8089 file URI, percent-encoding bytes
/// that are not UTF-8 rather than converting them lossily.
#[must_use]
pub fn file_uri(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

#[cfg(test)]
mod tests;
