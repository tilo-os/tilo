//! The page's `#[wasm_bindgen]` entry points once a mount is open: what the
//! page asks a mount (stranded writes, image and media requests) and what it
//! delivers into one (clipboard events, dropped files, decoded pictures,
//! visibility, frame metrics).

use super::{MOUNTS, admit_runtime, error, refresh_mounts, with_mount, with_mount_quietly};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use winit::event::WindowEvent;

/// Take accepted writes for the page to commit (`[{key, bytes, actor,
/// revision}]`).
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn pending_writes(id: u64) -> Result<JsValue, JsValue> {
    // The page persists after every presented status; storing what is
    // written changes nothing the tree shows.
    with_mount_quietly(id, |m| {
        m.flush_records();
        crate::storage::drain(&m.store)
    })
}

/// Recreate the surface, renderer and device without recreating documents.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn recover(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        // A GPU that failed drawing at full resolution recovers one size
        // smaller, rather than failing the same way again.
        if m.error.take().is_some() {
            m.caps = m.caps.after_gpu_failure();
            m.viewport.set_caps(m.caps);
        }
        m.remake_gpu(id);
        Ok(())
    })
}

/// Lose the mount's GPU device, as a driver reset or the browser does: what a
/// test of the page's recovery from a lost device asks for. The desk makes a
/// new device and draws on.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn lose_gpu(id: u64) -> Result<bool, JsValue> {
    with_mount(id, |m| {
        let Some(gpu) = &m.gpu else {
            return Ok(false);
        };
        let Ok(shared) = gpu.shared.try_borrow() else {
            return Ok(false);
        };
        let Some(handle) = shared.cx.devices.get(gpu.surface.dev_id) else {
            return Ok(false);
        };
        handle.device.destroy();
        Ok(true)
    })
}

/// Run a copy, cut or paste inside the trusted DOM clipboard event.
///
/// # Errors
///
/// Rejects an unknown kind or a closed mount.
#[wasm_bindgen]
pub fn clipboard_event(
    id: u64,
    kind: &str,
    transfer: JsValue,
    shift: bool,
) -> Result<bool, JsValue> {
    let key = match kind {
        "paste" => "v",
        "copy" => "c",
        "cut" => "x",
        _ => return Err(error("invalid clipboard event")),
    };
    crate::clipboard::during(transfer, || {
        with_mount(id, |m| {
            let mut modifiers =
                if m.frame.platform_conventions().platform() == ui::platform::Platform::MacOs {
                    winit::keyboard::ModifiersState::SUPER
                } else {
                    winit::keyboard::ModifiersState::CONTROL
                };
            if shift {
                modifiers |= winit::keyboard::ModifiersState::SHIFT;
            }
            m.frame.send_host_event(&WindowEvent::ModifiersChanged(
                winit::event::Modifiers::from(modifiers),
            ));
            // A failed clipboard sink must never let a cut delete the selection.
            if kind == "cut" {
                m.frame.key("c");
            }
            if kind != "cut" || crate::clipboard::copied() {
                m.frame.key(key);
            }
            m.frame.send_host_event(&WindowEvent::ModifiersChanged(
                winit::event::Modifiers::default(),
            ));
            m.pending = true;
            m.window.request_redraw();
            Ok(kind == "paste" || crate::clipboard::copied())
        })
    })
}

/// Largest file a drop hands an app, as every app's own limit is below it.
const MAX_DROPPED_FILE_BYTES: u32 = 64 * 1024 * 1024;
/// Most files one drop hands over.
const MAX_DROPPED_FILES: u32 = 16;

/// Hand the files the person dropped on the page at physical `(x, y)` of
/// the canvas to what is under it: `files` is `[{name, type, bytes}]`, each
/// read by the page from the drop, or `{name, size, unread}` for one it did
/// not read (`"tooLarge"` or `"tooMany"`), so what is under the pointer can
/// say why it took none of it. The pointer moves there, then each file
/// arrives as the `DroppedFile` a desktop sends, held for that dispatch
/// (`ui::portal::dropped`), since a page's files have no path. Returns how
/// many it handed over.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn drop_files(id: u64, files: &JsValue, x: f64, y: f64) -> Result<u32, JsValue> {
    let files = js_sys::Array::from(files);
    let read = |item: JsValue| {
        let field = |name: &str| js_sys::Reflect::get(&item, &name.into()).ok();
        let name = field("name")?.as_string().filter(|name| !name.is_empty())?;
        let unread = |why| {
            let size = field("size").and_then(|size| size.as_f64()).unwrap_or(0.0);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a file's size is a whole, non-negative number of bytes"
            )]
            let size = size.max(0.0) as u64;
            Some(ui::portal::Dropped::NotRead {
                name: name.clone(),
                size,
                why,
            })
        };
        match field("unread").and_then(|why| why.as_string()).as_deref() {
            Some("tooLarge") => return unread(ui::portal::NotRead::TooLarge),
            Some("tooMany") => return unread(ui::portal::NotRead::TooMany),
            _ => {}
        }
        let bytes = field("bytes")?.dyn_into::<js_sys::Uint8Array>().ok()?;
        if bytes.length() > MAX_DROPPED_FILE_BYTES {
            return unread(ui::portal::NotRead::TooLarge);
        }
        Some(ui::portal::Dropped::Contents(ui::portal::PickedContents {
            name: name.clone(),
            media_type: field("type")
                .and_then(|kind| kind.as_string())
                .filter(|kind| !kind.is_empty()),
            bytes: bytes.to_vec(),
            location: None,
        }))
    };
    // Past the most a drop hands over, one more says so.
    let dropped: Vec<_> = files
        .iter()
        .take(
            usize::try_from(MAX_DROPPED_FILES)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
        )
        .filter_map(read)
        .collect();
    with_mount(id, |m| {
        let (x, y) = m.viewport.map_pointer(x, y);
        m.frame.send_host_event(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        });
        let mut handed = 0_u32;
        for file in dropped {
            let path = std::path::PathBuf::from(match &file {
                ui::portal::Dropped::Contents(contents) => &contents.name,
                ui::portal::Dropped::NotRead { name, .. } => name,
            });
            ui::portal::dropping(file, || {
                m.frame.send_host_event(&WindowEvent::DroppedFile(path));
            });
            handed = handed.saturating_add(1);
        }
        m.pending = true;
        m.window.request_redraw();
        Ok(handed)
    })
}

/// The point of the desk at physical `(x, y)` of the canvas, in the surface's coordinates, and
/// what one of those is in the canvas's pixels.
fn desk_point(m: &super::Mount, x: f64, y: f64) -> (ui::kurbo::Point, f64) {
    let (mapped_x, mapped_y) = m.viewport.map_pointer(x, y);
    let dpr = m.viewport.applied().dpr.max(f64::MIN_POSITIVE);
    let (scale, _) = m.viewport.map_pointer(1.0, 1.0);
    (
        ui::kurbo::Point::new(mapped_x / dpr, mapped_y / dpr),
        dpr / scale.max(f64::MIN_POSITIVE),
    )
}

/// The desk's size in the surface's coordinates.
fn desk_size(m: &super::Mount) -> (f64, f64) {
    let applied = m.viewport.applied();
    let dpr = applied.dpr.max(f64::MIN_POSITIVE);
    (
        f64::from(applied.width) / dpr,
        f64::from(applied.height) / dpr,
    )
}

/// Where files dragged over physical `(x, y)` of the canvas would go: `{kind: "window", app,
/// takes, x, y, width, height}` (the window under the pointer, and whether its app opens files),
/// `{kind: "strip", x, y, width, height}` (the place between the strip's windows a window opens
/// at, drawn as a bar there), or `{kind: "refused", why}`. Places are in the canvas's pixels. A
/// mount that is one app, not a desk, is all window.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn drop_target(id: u64, x: f64, y: f64) -> Result<JsValue, JsValue> {
    with_mount_quietly(id, |m| {
        let answer = js_sys::Object::new();
        let set = |key: &str, value: JsValue| {
            let _ = js_sys::Reflect::set(&answer, &key.into(), &value);
        };
        let Some(desktop) = m.desktop.as_ref() else {
            set("kind", "window".into());
            set("takes", true.into());
            return Ok(answer.into());
        };
        let (point, px) = desk_point(m, x, y);
        let size = desk_size(m);
        let place = |rect: ui::kurbo::Rect| {
            set("x", (rect.x0 * px).into());
            set("y", (rect.y0 * px).into());
            set("width", (rect.width() * px).into());
            set("height", (rect.height() * px).into());
        };
        match desktop.drop_target(point, size) {
            super::drops::DropTarget::Window { rect, app, tab } => {
                set("kind", "window".into());
                if app.as_deref() == Some(fs_protocol::tree::FILES_APP) {
                    set("label", desktop.files_label(tab).into());
                }
                let takes = app.as_deref().is_some_and(super::drops::takes_files);
                set("takes", takes.into());
                set(
                    "hands",
                    app.as_deref().is_some_and(super::drops::hands_files).into(),
                );
                let name = app.as_deref().and_then(crate::desktop::known_application);
                set("app", name.map_or(JsValue::NULL, JsValue::from_str));
                place(rect);
            }
            super::drops::DropTarget::Strip { area, .. } => {
                set("kind", "strip".into());
                place(area);
            }
            super::drops::DropTarget::Refused(why) => {
                set("kind", "refused".into());
                set("why", why.into());
            }
        }
        Ok(answer.into())
    })
}

/// Copy `files` (`[{file, path}]`: a `File` or `Blob` the browser keeps, and its path in what was
/// dropped) dropped at physical `(x, y)` of the canvas into the workspace's file tree, and open
/// them where they fell ([`drop_target`]). `progress` is called with `{phase, done, total, text}`
/// as the copy goes: `phase` is `"copying"`, then `"done"` or `"failed"`, and `text` what to show
/// at the drop's place.
///
/// # Errors
///
/// Rejects a closed or single-application mount; where nothing takes files, why.
#[wasm_bindgen]
pub fn desktop_drop(
    id: u64,
    files: &JsValue,
    x: f64,
    y: f64,
    progress: js_sys::Function,
) -> Result<(), JsValue> {
    let files = super::uploads::Picked::all(files);
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let (point, _) = desk_point(m, x, y);
        let target = desktop.drop_target(point, desk_size(m));
        desktop
            .drop_files(files, &target, progress)
            .map_err(error)?;
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Put `files` (`[{file, path}]`, as [`desktop_drop`] takes them) chosen with the desk menu's
/// uploads into the workspace's file tree, telling `progress` how it goes as [`desktop_drop`]
/// does.
///
/// # Errors
///
/// Rejects a closed or single-application mount, a page in no workspace, and a person who may
/// not change the workspace's records.
#[wasm_bindgen]
pub fn desktop_upload(id: u64, files: &JsValue, progress: js_sys::Function) -> Result<(), JsValue> {
    let files = super::uploads::Picked::all(files);
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .upload(files, progress)
            .map_err(error)?;
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Image URLs the application asked for, as `[{loader, url, maxDimension}]`.
/// The page fetches and decodes each with the browser's own image decoders,
/// no larger than `maxDimension` on either side, and hands the pixels back
/// through [`complete_asset`].
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn asset_requests(id: u64) -> Result<String, JsValue> {
    // The page asks after every presented status; taking the image requests
    // changes nothing the tree shows (a loaded image wakes its own frame).
    with_mount_quietly(id, |m| {
        let mut tickets = m.image_tickets.borrow_mut();
        let mut loaders = m.images.borrow_mut();
        let mut wanted: Vec<(usize, String)> = m.blob_waiting.take();
        for (loader, images) in loaders.iter_mut().enumerate() {
            wanted.extend(images.take_requests().into_iter().map(|url| (loader, url)));
        }
        let mut requests = Vec::new();
        for (loader, url) in wanted {
            // Content the page lacks is asked of the workspace; the request waits
            // here until it has arrived.
            if !crate::page_content::image_ready(&m.image_owners, loader, &url) {
                m.blob_waiting.borrow_mut().push((loader, url));
                continue;
            }
            let Some(ticket) = loaders
                .get_mut(loader)
                .and_then(|images| images.reserve(&url))
            else {
                continue;
            };
            requests.push(serde_json::json!({
                "loader": loader,
                "url": url,
                "maxDimension": ticket.max_dimension(),
            }));
            tickets.insert((loader, url), ticket);
        }
        // What the page asked for goes to the workspace now, not on the next
        // frame an idle page may never draw.
        if !m.blob_waiting.borrow().is_empty()
            && let Some(peer) = m.peer
        {
            crate::replicated::ask_content(peer);
        }
        serde_json::to_string(&requests).map_err(error)
    })
}

/// The bytes of a content-addressed image request ([`asset_requests`]
/// hands one out only once they are here), for the page to decode.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn blob_asset(id: u64, loader: usize, url: &str) -> Result<Option<Vec<u8>>, JsValue> {
    with_mount_quietly(id, |m| {
        Ok(crate::page_content::image_bytes(
            &m.image_owners,
            loader,
            url,
        ))
    })
}

/// Deliver a requested image the page decoded: `width` × `height` pixels of
/// unpremultiplied 8-bit RGBA, or `None` when it could not be fetched or
/// decoded. Resolves whether the image was accepted.
///
/// # Errors
///
/// Rejects a closed mount or an unknown loader.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives JavaScript bytes only as an owned value"
)]
pub fn complete_asset(
    id: u64,
    loader: usize,
    url: &str,
    width: u32,
    height: u32,
    rgba: Option<Vec<u8>>,
) -> Result<bool, JsValue> {
    with_mount(id, |m| {
        let Some(ticket) = m
            .image_tickets
            .borrow_mut()
            .remove(&(loader, url.to_owned()))
        else {
            return Ok(false);
        };
        let complete = m
            .images
            .borrow_mut()
            .get_mut(loader)
            .ok_or_else(|| error("unknown image loader"))?
            .complete_decoded(
                &ticket,
                rgba.map(|rgba| ui::image_store::DecodedImage {
                    rgba,
                    width,
                    height,
                }),
            );
        // A decoded image changes what widgets measure and draw. The next
        // frame lays out once for every image that arrived before it: a page
        // of thumbnails decodes dozens in a row, and laying out after each
        // held the page's main thread for seconds.
        m.frame.request_layout();
        m.pending = true;
        m.window.request_redraw();
        Ok(complete)
    })
}

/// Publish the DOM event's modifier snapshot before winit dispatches its key.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "mirrors KeyboardEvent's four modifier flags"
)]
pub fn modifiers(
    id: u64,
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Result<(), JsValue> {
    use winit::keyboard::ModifiersState as M;
    let mut flags = M::empty();
    flags.set(M::CONTROL, control);
    flags.set(M::ALT, alt);
    flags.set(M::SHIFT, shift);
    flags.set(M::SUPER, meta);
    with_mount(id, |m| {
        m.frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(flags),
        ));
        Ok(())
    })
}

/// Follow `document.visibilityState`.
#[wasm_bindgen]
pub fn visible(id: u64, visible: bool) -> Result<(), JsValue> {
    admit_runtime()?;
    crate::host_services::set_hidden(!visible);
    MOUNTS.with_borrow_mut(|mounts| {
        if let Some(m) = mounts.get_mut(&id) {
            m.hidden = !visible;
            if visible {
                m.pending = true;
                m.window.request_redraw();
            }
        }
    });
    Ok(())
}

/// Names applications requested through `ui::host_data` since the last
/// call, as a JSON array, for the page to fetch from the bundle's `data/`.
///
/// # Errors
///
/// Propagates a serialization failure.
#[wasm_bindgen]
pub fn data_requests() -> Result<String, JsValue> {
    admit_runtime()?;
    serde_json::to_string(&ui::host_data::take_requests()).map_err(error)
}

/// Supply requested data, verified by the page against the bundle's
/// manifest, and relayout every mount so the requester sees it.
#[wasm_bindgen]
pub fn install_data(name: &str, bytes: Vec<u8>) -> Result<(), JsValue> {
    admit_runtime()?;
    ui::host_data::install(name, bytes);
    refresh_mounts();
    Ok(())
}

/// Report that requested data cannot be fetched.
#[wasm_bindgen]
pub fn data_failed(name: &str, reason: &str) -> Result<(), JsValue> {
    admit_runtime()?;
    ui::host_data::fail(name, reason);
    refresh_mounts();
    Ok(())
}

/// Opt-in bounded diagnostic capture; disabled by default and never persisted.
#[wasm_bindgen]
pub fn configure_frame_metrics(enabled: bool) -> Result<(), JsValue> {
    admit_runtime()?;
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut() {
            mount.metrics.borrow_mut().configure(enabled);
            if enabled {
                mount.frame.begin_profiling();
            } else {
                mount.frame.end_profiling();
            }
        }
    });
    Ok(())
}

/// Drain captured stage timings and per-pass layout profiles, per mount.
///
/// # Errors
///
/// Propagates a serialization failure.
#[wasm_bindgen]
pub fn take_frame_metrics() -> Result<String, JsValue> {
    admit_runtime()?;
    MOUNTS.with_borrow_mut(|mounts| {
        let samples: Vec<_> = mounts
            .iter_mut()
            .map(|(id, mount)| {
                let mut metrics = mount.metrics.borrow_mut();
                serde_json::json!({"mount": id, "samples": metrics.take(), "dropped": metrics.take_dropped(), "requesters": metrics.take_requesters(), "measuredBy": metrics.take_measured_by(), "damage": metrics.take_damage(), "layoutPasses": mount.frame.take_profile_samples(), "lastRelayout": format!("{:?}", mount.frame.last_relayout())})
            })
            .collect();
        serde_json::to_string(&samples).map_err(error)
    })
}

/// The shared Chat controls' local media requests, scoped to retained owners.
///
/// # Errors
///
/// Rejects a closed mount or a failure to serialize its requests.
#[wasm_bindgen]
pub fn take_media_requests(id: u64) -> Result<String, JsValue> {
    with_mount(id, |mount| {
        serde_json::to_string(&mount.media.take(&mut mount.frame)).map_err(error)
    })
}

/// Complete one exact retained component request; late replies are refused.
///
/// # Errors
///
/// Rejects malformed snapshots and closed mounts.
#[wasm_bindgen]
pub fn complete_media_request(id: u64, ticket: u64, response: &str) -> Result<bool, JsValue> {
    let result: Result<ui::native_media::Snapshot, String> =
        serde_json::from_str(response).map_err(error)?;
    if result.as_ref().is_ok_and(|snapshot| !snapshot.valid()) {
        return Err(error("invalid local media snapshot"));
    }
    with_mount(id, |mount| {
        let completed = mount.media.complete(&mut mount.frame, ticket, result);
        if completed {
            mount.window.request_redraw();
        }
        Ok(completed)
    })
}
