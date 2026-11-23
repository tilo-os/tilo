//! A window's own state: what its view declares (a calculator's mode, a book's open panels, a
//! list's filter, a sort order), kept in the window's entry of the workspace's layout so it
//! follows the window to its person's other devices. Drafts are none of it: they are content, and
//! live in a resource.
//!
//! The state has readers, by default the person whose window it is, and is sealed to their key
//! group ([`collab_types::readers`]): every member sees the window, only its readers what is in
//! its state. Its values are the tilolang runtime's JSON form of the view's typed `state`
//! declarations, each under its declaration's id, never its name in the source.
//!
//! Only the device that opened the window writes its state ([`write`]); any other device, an
//! agent or the palette asks that device with [`DesktopCommand::SetWindowState`], which it opens,
//! changes and seals again ([`apply`]). The shared desktop's guard refuses a write that changes
//! the state of someone else's window ([`crate::ownership::restated_windows`]).

use std::collections::BTreeMap;

use automerge::{AutoCommit, ChangeHash};
use collab_types::{Readers, WorkspaceId};
use key_group::ContentKeys;
use serde::{Deserialize, Serialize};

use crate::commands::DesktopCommand;
use crate::ids::TabId;
use crate::shared_resource::device::Writer;

/// A window's state as the layout keeps it: who reads it, and the state sealed to their group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedState {
    /// Who reads it: a reader set's text ([`Readers::text`]).
    pub readers: String,
    /// The [`WindowState`], sealed at the window's [`field`] by
    /// [`ContentKeys::seal_private_field`], in hex.
    pub sealed: String,
}

/// A window's state, opened: each declared field's value in the tilolang runtime's JSON form,
/// by the id of its `state` declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowState(pub BTreeMap<String, serde_json::Value>);

/// Why a window's state cannot be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// The text names no reader set ([`Readers::parse`]).
    #[error("{0:?} names no reader set")]
    Readers(String),
    /// The keys given are not the key group of the readers the state is sealed to.
    #[error("these keys are not the key group of the window's readers")]
    OtherGroup,
    /// The layout holds no such window.
    #[error("window {0} is not on the layout")]
    NoWindow(TabId),
    /// The window is another device's: only the one that opened it writes its state.
    #[error("window {0} was opened on another device, which alone writes its state")]
    NotTheMaker(TabId),
    /// The device writing names no person, whose window's state it would be.
    #[error("nobody is signed in to write the state of window {0}")]
    NoPerson(TabId),
    /// The command is not [`DesktopCommand::SetWindowState`].
    #[error("not a change of a window's state")]
    NotAStateCommand,
    /// A value, or a state opened, that is not JSON.
    #[error("a window's state is not JSON: {0}")]
    Json(String),
    /// The keys refused to seal or open it: not the readers' keys, or an epoch not held.
    #[error(transparent)]
    Keys(#[from] key_group::Error),
    /// The layout's document refused the write.
    #[error(transparent)]
    Document(#[from] automerge::AutomergeError),
}

/// The address window `tab`'s state is sealed at: its keys open it there and nowhere else.
#[must_use]
pub fn field(tab: TabId) -> String {
    format!("desktop/windows/{tab}/state")
}

/// The reader set of `person` alone, by their id (their root key in hex): whom a window's state
/// is sealed to until it is shared.
///
/// # Errors
///
/// [`StateError::Readers`] for an id that is not one person's root key.
pub fn person_readers(person: &str) -> Result<String, StateError> {
    match Readers::parse(person) {
        Some(Readers::People(people)) if people.len() == 1 => Ok(person.to_owned()),
        _ => Err(StateError::Readers(person.to_owned())),
    }
}

/// Seal `state` of window `tab` in `workspace` for `readers`, with `keys`, which must be their
/// group's ([`collab_types::reader_group`]).
///
/// # Errors
///
/// [`StateError::Readers`] or [`StateError::OtherGroup`] for readers the keys are not of, and
/// [`StateError::Keys`] when the keys fail to seal.
pub fn seal(
    keys: &ContentKeys,
    workspace: WorkspaceId,
    tab: TabId,
    readers: &str,
    state: &WindowState,
) -> Result<SealedState, StateError> {
    let group = collab_types::reader_group(workspace, readers)
        .ok_or_else(|| StateError::Readers(readers.to_owned()))?;
    if keys.workspace() != group {
        return Err(StateError::OtherGroup);
    }
    let bytes = serde_json::to_vec(state).map_err(|error| StateError::Json(error.to_string()))?;
    Ok(SealedState {
        readers: readers.to_owned(),
        sealed: keys.seal_private_field(&field(tab), &bytes)?,
    })
}

/// Open window `tab`'s `sealed` state with `keys`.
///
/// # Errors
///
/// [`StateError::Keys`] for keys of anyone but its readers, or sealed for another window.
pub fn open(
    keys: &ContentKeys,
    tab: TabId,
    sealed: &SealedState,
) -> Result<WindowState, StateError> {
    let bytes = keys.open_private_field(&field(tab), &sealed.sealed)?;
    serde_json::from_slice(&bytes).map_err(|error| StateError::Json(error.to_string()))
}

/// The key group window `tab`'s state in `workspace` is sealed under, as `doc` holds the
/// window: its readers', else those of `person`, whose window it is ([`readers`]). The keys
/// [`apply`] and [`read`] take are this group's.
///
/// # Errors
///
/// As [`readers`].
pub fn group(
    doc: &AutoCommit,
    workspace: WorkspaceId,
    tab: TabId,
    person: &str,
) -> Result<WorkspaceId, StateError> {
    let readers = readers(doc, tab, person)?;
    collab_types::reader_group(workspace, &readers).ok_or(StateError::Readers(readers))
}

/// Who reads window `tab`'s state: the readers it is sealed to, else `person` alone.
///
/// # Errors
///
/// [`StateError::Readers`] when neither names a reader set.
pub fn readers(doc: &AutoCommit, tab: TabId, person: &str) -> Result<String, StateError> {
    match sealed(doc, tab) {
        Some(sealed) => Readers::parse(&sealed.readers)
            .map(|_| sealed.readers.clone())
            .ok_or(StateError::Readers(sealed.readers)),
        None => person_readers(person),
    }
}

/// Each window's sealed state as the layout `doc` keeps it, by tab, as its text, now (`None`) or
/// as of `heads`: what the guard compares a write's states by.
pub(crate) fn states(doc: &AutoCommit, heads: Option<&[ChangeHash]>) -> BTreeMap<TabId, String> {
    let layout = match heads {
        Some(heads) => crate::desktop_doc::held_at(doc, heads),
        None => crate::desktop_doc::holds(doc),
    };
    if layout {
        crate::desktop_doc::window_states_at(doc, heads)
    } else {
        crate::shared_resource::window_states(doc, heads)
    }
}

/// Window `tab`'s sealed state in the layout `doc`; `None` while it has none.
#[must_use]
pub fn sealed(doc: &AutoCommit, tab: TabId) -> Option<SealedState> {
    states(doc, None)
        .remove(&tab)
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// Window `tab`'s state in the layout `doc`, opened with `keys`; `None` while it has none.
///
/// # Errors
///
/// As [`open`].
pub fn read(
    doc: &AutoCommit,
    keys: &ContentKeys,
    tab: TabId,
) -> Result<Option<WindowState>, StateError> {
    sealed(doc, tab)
        .map(|sealed| open(keys, tab, &sealed))
        .transpose()
}

/// Write `sealed` as window `tab`'s state in the layout `doc`, or take its state out (`None`),
/// as the device `writer` describes. Only the device that opened the window writes it. Whether
/// the layout changed; the caller commits, as for every write of the layout.
///
/// # Errors
///
/// [`StateError::NotTheMaker`] on any other device, [`StateError::NoWindow`] for a window the
/// layout does not hold, and [`StateError::Document`].
pub fn write(
    doc: &mut AutoCommit,
    writer: &Writer<'_>,
    tab: TabId,
    sealed: Option<&SealedState>,
) -> Result<bool, StateError> {
    if !(writer.mine)(tab) {
        return Err(StateError::NotTheMaker(tab));
    }
    let text = sealed
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| StateError::Json(error.to_string()))?;
    let wrote = if crate::desktop_doc::holds(doc) {
        crate::desktop_doc::put_window_state(doc, tab, text.as_deref())
    } else {
        crate::shared_resource::put_window_state(doc, tab, text.as_deref())?
    };
    wrote.ok_or(StateError::NoWindow(tab))
}

/// Apply `command`, a [`DesktopCommand::SetWindowState`] its asker was admitted for
/// ([`crate::commands::policy::may_set_window_state`]), on the device that opened the window:
/// open the window's state with `keys` (the group [`group`] names), set the field, and seal and
/// [`write`] it again. A field set to the value it holds writes nothing. Whether the layout
/// changed.
///
/// # Errors
///
/// [`StateError::NotAStateCommand`], [`StateError::NoPerson`], [`StateError::Json`] for a value
/// that is not JSON, and as [`open`], [`seal`] and [`write`].
pub fn apply(
    doc: &mut AutoCommit,
    writer: &Writer<'_>,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    command: &DesktopCommand,
) -> Result<bool, StateError> {
    let DesktopCommand::SetWindowState { tab, state, value } = command else {
        return Err(StateError::NotAStateCommand);
    };
    let tab = *tab;
    if !(writer.mine)(tab) {
        return Err(StateError::NotTheMaker(tab));
    }
    let person = writer.person.ok_or(StateError::NoPerson(tab))?;
    let value: serde_json::Value =
        serde_json::from_str(value).map_err(|error| StateError::Json(error.to_string()))?;
    let readers = readers(doc, tab, person)?;
    let mut current = read(doc, keys, tab)?.unwrap_or_default();
    if current.0.get(state) == Some(&value) {
        return Ok(false);
    }
    current.0.insert(state.clone(), value);
    let sealed = seal(keys, workspace, tab, &readers, &current)?;
    write(doc, writer, tab, Some(&sealed))
}

/// Give window `tab`'s state the readers `readers`: open it with `opened_with` (its readers'
/// keys now) and seal it again with `sealed_with`, the group of `readers`. What a share or a
/// follow does to a window's state; only the device that opened the window does it. Whether the
/// layout changed.
///
/// # Errors
///
/// As [`open`], [`seal`] and [`write`].
pub fn reseal(
    doc: &mut AutoCommit,
    writer: &Writer<'_>,
    workspace: WorkspaceId,
    tab: TabId,
    (opened_with, sealed_with): (&ContentKeys, &ContentKeys),
    readers: &Readers,
) -> Result<bool, StateError> {
    if !(writer.mine)(tab) {
        return Err(StateError::NotTheMaker(tab));
    }
    let state = read(doc, opened_with, tab)?.unwrap_or_default();
    let sealed = seal(sealed_with, workspace, tab, &readers.text(), &state)?;
    write(doc, writer, tab, Some(&sealed))
}
