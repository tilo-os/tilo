//! A person's file chooser, between the host and the embedder
//! ([`crate::webview::file_chooser`]).

use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::ChromiumEngine;
use crate::webview::file_chooser::{
    FileChooser, FileChooserMode, MAX_FILE_CHOOSER_TITLE, MAX_FILE_CHOOSER_TYPES, accepted_type,
};

/// Whether the embedder answers a person's file chooser, or the host keeps
/// its platform's panel.
///
/// On macOS the host shows the system's own open panel, which hands what the
/// person picked to the host's process, the one that reads the files. A
/// picker the app showed there would hand it to the app's process instead,
/// and nothing shows the engine could then read the pick, so the proven
/// panel stays. Everywhere else the host has no panel at all (its pages are
/// offscreen) and the desktop's picker, asked by the app, is the only one.
pub(super) const EMBEDDER_ANSWERS: bool = !cfg!(target_os = "macos");

impl ChromiumEngine {
    /// Tell the host this tab answers a person's file choosers, once it
    /// asked and the host said it holds them.
    pub(super) fn tell_file_choosers(&self) {
        let shared = &self.shared;
        if !shared.choosers_asked.load(Ordering::Acquire)
            || !shared.choosers_can.load(Ordering::Acquire)
            || shared.choosers_told.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if self
            .tx
            .push(json!({"op":"file_choosers","tab":self.tab,"enabled":true}))
            .is_err()
        {
            shared.choosers_told.store(false, Ordering::Release);
        }
    }
}

/// A `file_chooser` event: the chooser a person opened, within the host's
/// bounds.
pub(super) fn decode(value: &Value) -> Option<FileChooser> {
    let mode = match value["mode"].as_str()? {
        "open" => FileChooserMode::One,
        "open_multiple" => FileChooserMode::Several,
        "open_folder" => FileChooserMode::Folder,
        _ => return None,
    };
    let accept = value["accept"]
        .as_array()
        .filter(|accept| accept.len() <= MAX_FILE_CHOOSER_TYPES)?
        .iter()
        .map(|accepted| {
            accepted
                .as_str()
                .filter(|accepted| accepted_type(accepted))
                .map(str::to_owned)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(FileChooser {
        id: u32::try_from(value["id"].as_u64()?)
            .ok()
            .filter(|id| *id > 0 && i32::try_from(*id).is_ok())?,
        mode,
        title: value["title"]
            .as_str()
            .filter(|title| title.len() <= MAX_FILE_CHOOSER_TITLE)?
            .to_owned(),
        accept,
    })
}
