//! Files and folders dragged onto the desk from the computer's own file manager.
//!
//! While they are dragged the desk says where they would go ([`Desktop::drop_target`]): the window
//! under the pointer when its app opens files, the place between the strip's windows a window
//! would open at, or why nothing there takes them. Dropped, they are copied into the workspace's
//! file tree ([`crate::dropped_files`]) and opened where they fell: on the desk, each in the app
//! that opens it, in new windows side by side at that place, or Files for a folder or a file no
//! app opens; on a window, by that app, as its own Import takes a file. The page shows how far the
//! copy is at the drop's place, and says at the end what was added where.

use std::rc::Rc;

use fs_protocol::tree::{FILES_APP, Tree};
use ui::kurbo::{Point, Rect};
use wasm_bindgen::prelude::*;

use super::Desktop;
use super::uploads::Picked;
use crate::dropped_files::{self, DESK_FOLDER, Kept};
use crate::page_files::WorkspaceFiles;

/// How far the outline of a drop's place keeps from the desk's edges.
const DESK_INSET: f64 = 8.0;

/// The most of a file read from the browser at once while it is chunked: the page holds no more
/// of a file than this and one chunk.
const READ_PIECE: u64 = 4 * 1024 * 1024;

/// The workspace's tree, waiting while it is on its way from the workspace's hosts (they send it,
/// or say they have none and it is founded here).
async fn tree_here(files: &WorkspaceFiles) -> Result<ui::record::StoredRecord, String> {
    for _ in 0..TREE_WAITS {
        match files.tree_record() {
            Err(ui::record::RecordOpenError::Arriving | ui::record::RecordOpenError::Opening) => {
                super::frame_loop::sleep_ms(TREE_WAIT_MS).await;
            }
            opened => {
                return opened
                    .map_err(|error| format!("The workspace's files are not open here: {error}"));
            }
        }
    }
    Err("The workspace's files did not arrive from its host; try again in a moment".into())
}

/// How long a drop waits for the workspace's tree to arrive, in steps.
const TREE_WAITS: u32 = 120;
const TREE_WAIT_MS: u32 = 250;

/// The apps whose windows take the bytes of a file dropped on them, where a page hands them over
/// (`ui::portal::dropped`): a note takes a picture, a conversation shares a file, an edit takes a
/// clip. Another app that opens files takes one as its launch's Import, beside its window.
const TAKES_WINDOW_DROPS: [&str; 3] = ["so.tilo.notes", "so.tilo.chat", "so.tilo.video-editor"];

/// Whether a window of `app` takes the bytes of files dropped on it itself, as its window's drop
/// does: the page hands them to it besides copying them into the workspace's files.
pub(crate) fn hands_files(app: &str) -> bool {
    TAKES_WINDOW_DROPS.contains(&app)
}

/// Whether a window of `app` takes files dropped on it: by its own window's drop, or opened beside
/// it in the app as its Import takes a file.
pub(crate) fn takes_files(app: &str) -> bool {
    app == FILES_APP
        || TAKES_WINDOW_DROPS.contains(&app)
        || tilo_content_types::landing_in(app).is_some()
}

/// Where files dropped at a point of the desk go.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DropTarget {
    /// The window of `tab`, standing at `rect`, whose app is `app`.
    Window {
        tab: u64,
        rect: Rect,
        app: Option<String>,
    },
    /// Between the strip's windows, or anywhere on a desk with none: a window opens at `slot`,
    /// and `area` is what the page outlines (a bar in the gap, or the whole empty desk).
    Strip {
        slot: tilo_desktop_surface::StripSlot,
        area: Rect,
    },
    /// Nothing there takes files, and why.
    Refused(String),
}

impl Desktop {
    /// Where files dropped at `point` (the surface's coordinates) go, the desk being `width` ×
    /// `height`.
    pub(super) fn drop_target(&self, point: Point, (width, height): (f64, f64)) -> DropTarget {
        if !self.may_upload() {
            return DropTarget::Refused(if self.shared.peer.is_some() {
                "You can view this workspace, but not add files to it".into()
            } else {
                "Open a workspace to add files to it".into()
            });
        }
        let window = self
            .handle
            .float_covers()
            .into_iter()
            .chain(
                self.handle
                    .shown_tabs()
                    .into_iter()
                    .map(|(tab, rect)| (tab, self.handle.window_rect(tab).unwrap_or(rect))),
            )
            .find(|(_, rect)| rect.contains(point));
        if let Some((tab, rect)) = window {
            return DropTarget::Window {
                tab,
                rect,
                app: self.handle.app_of(tab),
            };
        }
        let top = f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX) + DESK_INSET;
        let bottom = (height - DESK_INSET).max(top);
        match self.handle.slot_at(point.x) {
            Some(slot) if self.handle.shown_tabs().is_empty() => DropTarget::Strip {
                slot,
                area: Rect::new(
                    DESK_INSET,
                    top,
                    (width - DESK_INSET).max(DESK_INSET),
                    bottom,
                ),
            },
            Some(slot) => DropTarget::Strip {
                slot,
                area: Rect::new(slot.x - 3.0, top, slot.x + 3.0, bottom),
            },
            None => DropTarget::Refused("Open a workspace to add files to it".into()),
        }
    }

    /// Copy `files` dropped at `point` (`target`, as [`Self::drop_target`] answered) into the
    /// workspace's files and open them there, telling `progress` how it goes.
    ///
    /// # Errors
    ///
    /// Where nothing takes files, why.
    pub(super) fn drop_files(
        &self,
        files: Vec<Picked>,
        target: &DropTarget,
        progress: js_sys::Function,
    ) -> Result<(), String> {
        let opens = match target {
            DropTarget::Refused(why) => return Err(why.clone()),
            DropTarget::Strip { slot, .. } => Opens::Strip(slot.index),
            DropTarget::Window { tab, app, .. } => match app.as_deref() {
                // Files takes them into the folder it shows, and shows them there.
                Some(FILES_APP) => Opens::Nothing,
                Some(app) if hands_files(app) => {
                    // The page hands the bytes to the focused window once it has read them.
                    self.handle.focus_tab(*tab);
                    Opens::Nothing
                }
                Some(app) if tilo_content_types::landing_in(app).is_some() => {
                    Opens::Beside(*tab, app.to_owned())
                }
                app => {
                    let name = app
                        .and_then(crate::desktop::known_application)
                        .unwrap_or("This window");
                    return Err(format!("{name} doesn't open files"));
                }
            },
        };
        let into = match target {
            DropTarget::Window { tab, app, .. } if app.as_deref() == Some(FILES_APP) => {
                self.files_folder(*tab)
            }
            _ => None,
        };
        self.start(files, opens, into, progress)
    }

    /// The folder of the workspace's tree the Files window of `tab` shows, by its node; `None`
    /// for the tree's root.
    fn files_folder(&self, tab: u64) -> Option<String> {
        let checkout = self.shared.record_of(tab)?;
        let store = self.files.as_ref()?.records().store_of(FILES_APP).ok()?;
        store
            .read(&checkout.id, |doc| {
                use ui::automerge::ReadDoc as _;
                match doc.get(ui::automerge::ROOT, "folder") {
                    Ok(Some((ui::automerge::Value::Scalar(value), _))) => match value.as_ref() {
                        ui::automerge::ScalarValue::Str(folder) => Some(folder.to_string()),
                        _ => None,
                    },
                    _ => None,
                }
            })
            .flatten()
            .filter(|folder| !folder.is_empty() && folder != fs_protocol::tree::ROOT_ID)
    }

    /// What hovering files over the Files window of `tab` says: the folder they go into.
    pub(super) fn files_label(&self, tab: u64) -> String {
        let named = self.files_folder(tab).and_then(|folder| {
            let page = self.files.as_ref()?;
            let tree = page.tree_record().ok()?.read(|doc| doc.clone().save())?;
            let tree = Tree::of_record(&tree).ok()?;
            let node = tree.node(&folder)?;
            Some(String::from_utf8_lossy(node.name.as_bytes()).into_owned())
        });
        named.map_or_else(
            || "Add to the workspace's files".to_owned(),
            |name| format!("Add to {name}"),
        )
    }

    /// Copy `files` into the workspace's files, opening nothing: what the desk menu's uploads
    /// choose.
    ///
    /// # Errors
    ///
    /// A page in no workspace.
    pub(super) fn add_files(
        &self,
        files: Vec<Picked>,
        progress: js_sys::Function,
    ) -> Result<(), String> {
        self.start(files, Opens::Nothing, None, progress)
    }

    /// Copy `files` into the folder `into` of the tree (its root for `None`), then open them as
    /// `opens` says.
    fn start(
        &self,
        files: Vec<Picked>,
        opens: Opens,
        into: Option<String>,
        progress: js_sys::Function,
    ) -> Result<(), String> {
        let Some(page) = self.files.clone() else {
            return Err("Open a workspace to add files to it".into());
        };
        if files.is_empty() {
            return Ok(());
        }
        let handle = self.handle.clone();
        let names = Rc::clone(&self.shared.content_names);
        let wake = self.shared.clone();
        wasm_bindgen_futures::spawn_local(async move {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a drop is far below the 2^53 bytes a number holds exactly"
            )]
            let report = |phase: &str, done: u64, total: u64, text: &str| {
                let state = js_sys::Object::new();
                for (key, value) in [
                    ("phase", JsValue::from_str(phase)),
                    ("done", JsValue::from_f64(done as f64)),
                    ("total", JsValue::from_f64(total as f64)),
                    ("text", JsValue::from_str(text)),
                ] {
                    let _ = js_sys::Reflect::set(&state, &key.into(), &value);
                }
                let _ = progress.call1(&JsValue::NULL, &state);
            };
            match copy(&page, &files, into.as_deref(), &report).await {
                Ok(copied) => {
                    let opened = open(&handle, &names, &copied, &opens);
                    let mut line = dropped_files::added_line(
                        &copied.top,
                        copied.files.len(),
                        copied.folder_name.as_deref(),
                    );
                    if let Some(opened) = opened {
                        line = format!("{line} · {opened}");
                    }
                    report("done", copied.bytes, copied.bytes, &line);
                }
                Err(why) => report("failed", 0, 0, &why),
            }
            wake.wake();
        });
        Ok(())
    }
}

/// What opens the files of a drop once they are copied.
enum Opens {
    /// New windows side by side from the strip's place `index`.
    Strip(usize),
    /// Tabs beside the window of the tab, in its app: an app that takes a file at its launch as
    /// its Import does.
    Beside(u64, String),
    /// The window they fell on took them itself.
    Nothing,
}

/// A copied drop: each file as it landed, with what it is.
struct Copied {
    files: Vec<CopiedFile>,
    top: Vec<String>,
    folder: bool,
    bytes: u64,
    /// The name of the folder they went into; `None` for the tree's root.
    folder_name: Option<String>,
}

struct CopiedFile {
    name: String,
    hash: String,
    /// The app that opens it, when one does.
    app: Option<String>,
}

/// Keep each of `files` in the page's content as its manifest and chunks, then put them all into
/// the folder `into` of the tree (its root for `None`, or when the folder is gone) in one change:
/// what the drop added, every device sees at once.
async fn copy(
    page: &WorkspaceFiles,
    files: &[Picked],
    into: Option<&str>,
    report: &dyn Fn(&str, u64, u64, &str),
) -> Result<Copied, String> {
    let total: u64 = files.iter().map(Picked::size).sum();
    let title = match files {
        [one] => format!("Adding {}", one.path()),
        all => format!("Adding {}", dropped_files::files(all.len())),
    };
    report("copying", 0, total, &title);
    let mut kept = Vec::with_capacity(files.len());
    let mut apps = Vec::with_capacity(files.len());
    let mut done = 0_u64;
    for file in files {
        let mut head = Vec::new();
        let hash = keep(page.content(), file, &mut head, &mut |at| {
            report("copying", done.saturating_add(at), total, &title);
        })
        .await
        .map_err(|why| format!("{} was not added: {why}", file.path()))?;
        done = done.saturating_add(file.size());
        apps.push(dropped_files::opens_in(&head, file.path()));
        kept.push(Kept {
            path: file.path().to_owned(),
            hash,
            size: file.size(),
        });
    }
    let record = tree_here(page).await?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the clock reads whole, non-negative milliseconds"
    )]
    let mtime = js_sys::Date::now().max(0.0) as u64;
    let author = page.records().author().0.clone();
    let (placed, folder_name) = record.edit(false, |doc| {
        let mut tree = Tree::of_record(&doc.save()).map_err(|error| error.to_string())?;
        let folder = into
            .and_then(|folder| {
                tree.node(folder)
                    .filter(|_| !tree.trashed(folder))
                    .map(|node| (folder, node))
            })
            .map(|(folder, node)| {
                (
                    folder.to_owned(),
                    String::from_utf8_lossy(node.name.as_bytes()).into_owned(),
                )
            });
        let at = folder
            .as_ref()
            .map_or(DESK_FOLDER, |(folder, _)| folder.as_str());
        dropped_files::place(doc, &mut tree, at, &kept, (mtime, &author))
            .map(|placed| (placed, folder.map(|(_, name)| name)))
    })?;
    let folder = kept.iter().any(|file| file.path.contains('/'));
    Ok(Copied {
        files: placed
            .files
            .into_iter()
            .zip(kept)
            .zip(apps)
            .map(|((landed, kept), app)| CopiedFile {
                name: landed.name,
                hash: kept.hash,
                app,
            })
            .collect(),
        top: placed.top,
        folder,
        bytes: total,
        folder_name,
    })
}

/// Keep `file` in `content` as its chunks and manifest, read a piece at a time, telling `read`
/// how many of its bytes were kept; its first bytes go into `head`. Its manifest's hash (hex).
async fn keep(
    content: &tilo_content::ContentStore,
    file: &Picked,
    head: &mut Vec<u8>,
    read: &mut dyn FnMut(u64),
) -> Result<String, String> {
    let size = file.size();
    let mut stream = fs_content::Streamed::new(size, None);
    let mut refused: Option<String> = None;
    let mut store = |_: &fs_content::ChunkRef, bytes: &[u8]| {
        if refused.is_none()
            && let Err(error) = content.put_public(bytes)
        {
            refused = Some(error.to_string());
        }
    };
    let mut at = 0_u64;
    while at < size {
        let to = at.saturating_add(READ_PIECE).min(size);
        let bytes = file.read(at, to).await?;
        if head.is_empty() {
            head.extend_from_slice(
                bytes
                    .get(..bytes.len().min(tilo_content_types::HEAD_BYTES))
                    .unwrap_or_default(),
            );
        }
        stream
            .push(&bytes, &mut store)
            .map_err(|error| error.to_string())?;
        at = to;
        read(at);
    }
    let (manifest, _) = stream
        .finish(&mut store)
        .map_err(|error| error.to_string())?;
    if let Some(refused) = refused {
        return Err(refused);
    }
    content
        .put_public(&manifest.encode())
        .map_err(|error| error.to_string())
}

/// Open the files of `copied` as `opens` says; what opened, as the drop's end says it.
fn open(
    handle: &tilo_desktop_surface::DesktopHandle,
    names: &std::cell::RefCell<std::collections::HashMap<String, String>>,
    copied: &Copied,
    opens: &Opens,
) -> Option<String> {
    let launch = |app: &str, file: &CopiedFile| {
        names
            .borrow_mut()
            .insert(file.hash.clone(), file.name.clone());
        tilo_desktop_surface::session::content_launch(app, &file.hash)
    };
    match opens {
        Opens::Nothing => None,
        Opens::Beside(tab, app) => {
            let mut opened = 0_usize;
            for file in copied
                .files
                .iter()
                .filter(|file| file.app.as_deref() == Some(app))
            {
                let asked =
                    handle.dispatch(tilo_desktop_surface::navigation::ChromeEffect::OpenApp {
                        app: launch(app, file),
                        beside: Some(tab.to_string()),
                        background: None,
                        float: None,
                    });
                opened = opened.saturating_add(usize::from(asked.is_ok()));
            }
            let name = crate::desktop::known_application(app).unwrap_or(app);
            Some(if opened == copied.files.len() {
                format!("opened in {name}")
            } else {
                format!("{} opened in {name}", dropped_files::files(opened))
            })
        }
        Opens::Strip(index) => {
            // A folder opens as one window of Files; each file in its own app, side by side, and
            // Files for one no app here opens.
            let files_here = crate::desktop::known_application(FILES_APP).is_some();
            let mut at = *index;
            let mut opened = 0_usize;
            if copied.folder {
                if files_here && handle.open_at(FILES_APP, at).is_ok() {
                    return Some("opened in Files".into());
                }
                return None;
            }
            for file in &copied.files {
                let app = file
                    .app
                    .as_deref()
                    .filter(|app| crate::desktop::known_application(app).is_some());
                let asked = match app {
                    Some(app) => handle.open_at(&launch(app, file), at),
                    None if files_here => handle.open_at(FILES_APP, at),
                    None => continue,
                };
                if asked.is_ok() {
                    opened = opened.saturating_add(1);
                    at = at.saturating_add(1);
                }
            }
            (opened > 0).then(|| {
                if opened == copied.files.len() {
                    "opened here".to_owned()
                } else {
                    format!("{} opened here", dropped_files::files(opened))
                }
            })
        }
    }
}
