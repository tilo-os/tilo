//! Files and folders a person puts into the workspace from the page with the desk menu's
//! "Upload files…" and "Upload a folder…": copied into the workspace's file tree as a drop on the
//! desk is ([`super::drops`]), from where the browser keeps them, a piece at a time.

use std::cell::RefCell;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;

use super::Desktop;

/// The desk menu's row that chooses files to upload.
pub(crate) const UPLOAD_FILES: &str = "upload-files";
/// The desk menu's row that chooses a folder to upload.
pub(crate) const UPLOAD_FOLDER: &str = "upload-folder";

/// One file the page hands over: its bytes where the browser keeps them (a `File` or `Blob`),
/// and its path in what was dropped or chosen (`docs/2026/plan.md`).
pub(super) struct Picked {
    blob: JsValue,
    path: String,
    size: u64,
}

impl Picked {
    /// `[{file, path}]` as the page hands them over; an item that is not one is skipped.
    pub(super) fn all(files: &JsValue) -> Vec<Self> {
        js_sys::Array::from(files)
            .iter()
            .filter_map(|item| {
                let field = |name: &str| js_sys::Reflect::get(&item, &name.into()).ok();
                let blob = field("file").filter(JsValue::is_object)?;
                let size = js_sys::Reflect::get(&blob, &"size".into())
                    .ok()
                    .and_then(|size| size.as_f64())?;
                let path = field("path")
                    .and_then(|path| path.as_string())
                    .filter(|path| !path.is_empty())
                    .or_else(|| {
                        js_sys::Reflect::get(&blob, &"name".into())
                            .ok()
                            .and_then(|name| name.as_string())
                    })?;
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a file's size is a whole, non-negative number of bytes"
                )]
                let size = size.max(0.0) as u64;
                Some(Self { blob, path, size })
            })
            .collect()
    }
}

/// The bytes `from..to` of `blob`, read only now: a file of hundreds of megabytes is never held
/// whole by the page.
async fn read(blob: &JsValue, from: u64, to: u64) -> Result<Vec<u8>, String> {
    let call = |on: &JsValue, name: &str| {
        js_sys::Reflect::get(on, &name.into())
            .ok()
            .and_then(|method| method.dyn_into::<js_sys::Function>().ok())
            .ok_or_else(|| format!("the browser offers no {name} here"))
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a file is far below the 2^53 bytes a number holds exactly"
    )]
    let slice = call(blob, "slice")?
        .call2(blob, &JsValue::from(from as f64), &JsValue::from(to as f64))
        .map_err(|error| format!("{error:?}"))?;
    let promise = call(&slice, "arrayBuffer")?
        .call0(&slice)
        .map_err(|error| format!("{error:?}"))?
        .dyn_into::<js_sys::Promise>()
        .map_err(|_| "the browser read no bytes".to_owned())?;
    let buffer = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|error| {
            error
                .as_string()
                .unwrap_or_else(|| "the browser could not read the file".into())
        })?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

impl Picked {
    /// Its length in bytes.
    pub(super) fn size(&self) -> u64 {
        self.size
    }

    /// Its path in what was dropped or chosen.
    pub(super) fn path(&self) -> &str {
        &self.path
    }

    /// Its bytes `from..to`, read now.
    pub(super) async fn read(&self, from: u64, to: u64) -> Result<Vec<u8>, String> {
        read(&self.blob, from, to).await
    }
}

/// The desk menu's upload rows, offered only to a person who may change the workspace's records.
pub(super) fn host_actions() -> [tilo_desktop_surface::HostAction; 2] {
    let row =
        |id: &str, title: &str, subtitle: &str, icon: &str| tilo_desktop_surface::HostAction {
            id: id.to_owned(),
            title: title.to_owned(),
            subtitle: Some(subtitle.to_owned()),
            icon: Some(icon.to_owned()),
            destructive: false,
            unasked: true,
            source: Some(tilo_desktop_surface::HostActionSource::Workspace),
        };
    [
        row(
            UPLOAD_FILES,
            "Upload files…",
            "Put files from this computer into the workspace",
            "upload",
        ),
        row(
            UPLOAD_FOLDER,
            "Upload a folder…",
            "Put a folder and everything in it into the workspace",
            "folder",
        ),
    ]
}

/// What the page's own rows are and which of them the person was last offered: the upload rows
/// come and go with whether they may change the workspace's records.
#[derive(Default)]
pub(super) struct Offered {
    /// The rows the page asked the desk to offer.
    page: RefCell<Vec<tilo_desktop_surface::HostAction>>,
    /// Whether the rows offered now include the upload rows; `None` before the first offer.
    uploads: RefCell<Option<bool>>,
}

impl Desktop {
    /// Whether this page's person may put files into the workspace: a page in a workspace, whose
    /// person may change its records there.
    pub(super) fn may_upload(&self) -> bool {
        self.shared.peer.is_some()
            && self
                .shared
                .standing
                .role()
                .is_some_and(|role| role.includes(tilo_action_policy::Role::Editor))
    }

    /// Offer `rows`, the page's own, on the desk, with the upload rows where the person may
    /// upload. What the desk no longer offers is withdrawn from what still asks for it. Whether
    /// anything was withdrawn.
    pub(super) fn offer_host_actions(&self, rows: Vec<tilo_desktop_surface::HostAction>) -> bool {
        *self.offered.page.borrow_mut() = rows;
        self.offer_uploads(true)
    }

    /// Offer the upload rows again when whether the person may upload changed since they were
    /// last offered (`always`: offer the rows whatever changed). Whether anything was withdrawn.
    pub(super) fn offer_uploads(&self, always: bool) -> bool {
        let uploads = self.may_upload();
        if !always && *self.offered.uploads.borrow() == Some(uploads) {
            return false;
        }
        *self.offered.uploads.borrow_mut() = Some(uploads);
        let mut rows = self.offered.page.borrow().clone();
        if uploads {
            rows.extend(host_actions());
        }
        let mut withdrew = false;
        for gone in self.handle.set_host_actions(rows) {
            withdrew |= self.persistence.notifications.withdraw_host_action(&gone);
        }
        withdrew
    }

    /// Copy `files` into the workspace's file tree, telling `progress` how it goes.
    ///
    /// # Errors
    ///
    /// A page in no workspace, or a person who may not change its records.
    pub(super) fn upload(
        &self,
        files: Vec<Picked>,
        progress: js_sys::Function,
    ) -> Result<(), String> {
        if !self.may_upload() {
            // Said where the files were dropped, not only to the page.
            let why = "You can't put files into this workspace";
            self.handle.acknowledge(why);
            return Err(why.into());
        }
        self.add_files(files, progress)
    }
}
