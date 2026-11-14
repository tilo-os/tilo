//! Files and folders a person puts into the workspace from the page: dropped on the desk, or
//! chosen with the desk menu's "Upload files…" and "Upload a folder…".
//!
//! Each file goes up through the page's session to the workspace's host, a piece at a time read
//! from where the browser keeps it ([`tilo_session_core::remote::upload`]), and lands in the app
//! that opens what it is, by the workspace's own `open` of that app: what the CLI and an agent
//! do too. The record it landed in is shown on the workspace's shared desk the way an agent's
//! work is ([`upload::show_request`]), so everyone in the workspace sees each file arrive, beside
//! what they were doing rather than over it. One system job says how far it got, names the
//! file going up, can be stopped, and ends saying what landed where and what did not.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use tilo_actions::remote::Caller;
use tilo_session_core::remote::SessionActions;
use tilo_session_core::remote::upload::{self, Landed, Source, UploadError};
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

impl Source for Picked {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(
        &self,
        from: u64,
        to: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + '_>> {
        Box::pin(read(&self.blob, from, to))
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
    fn may_upload(&self) -> bool {
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

    /// Put `files` into the workspace and land each in the app that opens it, as one job the
    /// person can follow and stop.
    ///
    /// # Errors
    ///
    /// A page in no workspace, or a person who may not change its records.
    pub(super) fn upload(&self, files: Vec<Picked>) -> Result<(), String> {
        if !self.may_upload() {
            // Said where the files were dropped, not only to the page.
            let why = "You can't put files into this workspace";
            self.handle.acknowledge(why);
            return Err(why.into());
        }
        let workspace = self.shared.workspace.clone();
        let door = self
            .shared
            .peer
            .zip(crate::peer::workspace_id(&workspace).ok())
            .and_then(|(peer, id)| crate::peer_sync::actions_of(peer, id))
            .ok_or_else(|| "This page holds no session with the workspace's host".to_owned())?;
        if files.is_empty() {
            return Ok(());
        }
        let caller = self.caller();
        let shared = self.shared.clone();
        wasm_bindgen_futures::spawn_local(run(door, caller, workspace, files, move || {
            shared.wake();
        }));
        Ok(())
    }
}

/// How the job names `files` going up.
fn title(files: &[Picked]) -> String {
    match files {
        [one] => format!("Uploading {}", one.path),
        all => format!("Uploading {} files", all.len()),
    }
}

/// How a file went.
enum Went {
    /// It landed in the app `app` (its package).
    Landed { app: String },
    /// No app opens what it is: it is kept as content of the workspace.
    Kept,
    /// It did not go up, and why.
    Failed(String),
}

async fn run(
    door: SessionActions,
    caller: Caller,
    workspace: String,
    files: Vec<Picked>,
    wake: impl Fn(),
) {
    let total: u64 = files.iter().map(|file| file.size).sum();
    let job = ui::jobs::start(
        ui::jobs::Spec::new(title(&files))
            .unit(ui::jobs::Unit::Bytes)
            .stoppable(|| {}),
    );
    job.progress(0, total);
    let mut went: Vec<(String, Went)> = Vec::new();
    let mut base = 0_u64;
    for file in &files {
        if job.is_stopped() {
            break;
        }
        job.phase(file.path.clone());
        wake();
        let outcome = one(
            &door,
            &caller,
            &workspace,
            file,
            (base, total),
            (&job, &wake),
        )
        .await;
        base = base.saturating_add(file.size);
        job.progress(base, total);
        match outcome {
            Err(UploadError::Stopped) => break,
            Err(error) => went.push((file.path.clone(), Went::Failed(error.to_string()))),
            Ok(outcome) => went.push((file.path.clone(), outcome)),
        }
    }
    let told = outcome_line(&went, files.len());
    if job.is_stopped() {
        job.stopped();
    } else if went.iter().all(|(_, went)| matches!(went, Went::Failed(_))) {
        job.failed(told);
    } else {
        job.done(told);
    }
    wake();
}

/// Put `file` up and land it: how it went. `base` of `total` bytes went up before it.
async fn one(
    door: &SessionActions,
    caller: &Caller,
    workspace: &str,
    file: &Picked,
    (base, total): (u64, u64),
    (job, wake): (&ui::jobs::Running, &dyn Fn()),
) -> Result<Went, UploadError> {
    let head = read(&file.blob, 0, file.size.min(upload::HEAD_BYTES as u64))
        .await
        .map_err(UploadError::Read)?;
    let content = upload::put(
        door,
        file,
        &mut |at| {
            job.progress(base.saturating_add(at), total);
            wake();
        },
        &|| job.is_stopped(),
    )
    .await?;
    let Some(request) = upload::landing_request(workspace, &head, &file.path, &content) else {
        return Ok(Went::Kept);
    };
    let app = request
        .target
        .address
        .resource
        .trim_start_matches(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
        .to_owned();
    let answer = upload::settled(door, caller, request)
        .await
        .map_err(|why| {
            UploadError::Refused(tilo_actions::remote::ActionHostError::Refused { reason: why })
        })?;
    // On the workspace's desk, beside what everyone is doing: a window nobody has to make room
    // for, which they see arrive.
    if let Some(landed) = answer.as_ref().and_then(Landed::of)
        && let Err(why) =
            upload::settled(door, caller, upload::show_request(workspace, &landed)).await
    {
        tracing::warn!(
            "the uploaded {} landed but is not on the desk: {why}",
            file.path
        );
    }
    Ok(Went::Landed { app })
}

/// What the job says at its end: what landed where, what is kept as it is, and what did not go
/// up and why.
fn outcome_line(went: &[(String, Went)], asked: usize) -> String {
    let mut by_app: BTreeMap<String, usize> = BTreeMap::new();
    let mut kept = 0_usize;
    let mut failed: Vec<String> = Vec::new();
    for (path, went) in went {
        match went {
            Went::Landed { app } => {
                let name = crate::desktop::known_application(app)
                    .map_or_else(|| app.clone(), str::to_owned);
                let count = by_app.entry(name).or_default();
                *count = count.saturating_add(1);
            }
            Went::Kept => kept = kept.saturating_add(1),
            Went::Failed(why) => failed.push(format!("{path} ({why})")),
        }
    }
    let files = |count: usize| {
        if count == 1 {
            "1 file".to_owned()
        } else {
            format!("{count} files")
        }
    };
    let mut parts: Vec<String> = by_app
        .iter()
        .map(|(app, count)| format!("{} in {app}", files(*count)))
        .collect();
    if kept > 0 {
        parts.push(format!("{} kept as is, as no app opens them", files(kept)));
    }
    let skipped = asked.saturating_sub(went.len());
    if skipped > 0 {
        parts.push(format!("{} not uploaded", files(skipped)));
    }
    if !failed.is_empty() {
        parts.push(format!("not uploaded: {}", failed.join(", ")));
    }
    let mut line = parts.join("; ");
    if let Some(first) = line.get(..1) {
        line = format!(
            "{}{}",
            first.to_uppercase(),
            line.get(1..).unwrap_or_default()
        );
    }
    line
}
