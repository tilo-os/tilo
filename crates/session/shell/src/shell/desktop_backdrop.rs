//! The desktop backdrop with this device's wallpapers.
//!
//! Drawing is the shared [`DesktopBackdrop`]; this module supplies the
//! wallpaper each workspace shows. The person's `appearance.wallpaper`
//! setting chooses it ([`tilo_wallpapers::choice`]): one of the pictures the
//! image ships, decoded once and kept, or their own picture, read from
//! Tilo's own 0600 registry and cache (the portal's preview file first).
//! A custom wallpaper is per device.
//!
//! Where a picture shows (desktop, lock screen) and on which workspaces is
//! the private registry's: a picture of the person's own and a shipped one
//! alike. When the person changes the wallpaper, "Show on" or "Use for" in
//! Settings, [`follow_wallpaper_choice`] applies the wallpaper they have to
//! what they chose, on the workspace they are on, so the change shows at
//! once rather than with the next picture.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};
pub use tilo_desktop::backdrop::DesktopBackdrop;
use vello::peniko::ImageData;

/// The backdrop for one local workspace, showing its native wallpaper.
pub fn native_backdrop(workspace_id: impl Into<String>) -> DesktopBackdrop {
    DesktopBackdrop::for_workspace(workspace_id).with_wallpaper_provider(native_wallpaper)
}

/// A backdrop that shows only the shipped wallpaper the person chose, for a
/// surface that must not read this device's private wallpaper registry (a
/// scratch scene, a capture, a desktop with no workspace yet).
pub fn shipped_backdrop(backdrop: DesktopBackdrop) -> DesktopBackdrop {
    backdrop.with_wallpaper_provider(|_| shipped_wallpaper())
}

/// The person's `appearance.wallpaper`, as this compositor last read it.
/// Unset until settings are followed: offscreen renders show the default.
static CHOICE: RwLock<Option<String>> = RwLock::new(None);

/// The wallpaper the person has and where they want it, as last applied:
/// `appearance.wallpaper`, `wallpaper_target` and `wallpaper_scope`.
type WallpaperIntent = (Option<String>, String, String);

/// The intent last seen; `None` before the first settings read, which only
/// notes it (a session start applies nothing).
static INTENT: Mutex<Option<WallpaperIntent>> = Mutex::new(None);

/// Take the `appearance.wallpaper` value from the settings `document`.
/// Called whenever the followed settings change; cheap when the value did
/// not, and the picture it names is decoded only when it is drawn. When the
/// wallpaper, where it shows or which workspaces it covers changed, the
/// wallpaper the person has is applied to their choice on `active`, the
/// workspace they are on, off this thread.
pub fn follow_wallpaper_choice(
    document: &serde_json::Map<String, serde_json::Value>,
    active: Option<&str>,
) {
    let value = settings_store::appearance::WALLPAPER.from_document(document);
    if let Ok(mut choice) = CHOICE.write()
        && *choice != value
    {
        choice.clone_from(&value);
    }
    let appearance = document
        .get("appearance")
        .and_then(serde_json::Value::as_object);
    let leaf = |name: &str, default: &str| {
        appearance
            .and_then(|section| section.get(name))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(default)
            .to_owned()
    };
    let intent = (
        value,
        leaf("wallpaper_target", "background"),
        leaf("wallpaper_scope", "workspace"),
    );
    let changed = {
        let Ok(mut last) = INTENT.lock() else {
            return;
        };
        let changed = last.as_ref().is_some_and(|last| *last != intent);
        *last = Some(intent.clone());
        changed
    };
    if let (true, Some(workspace)) = (changed, active) {
        apply_aside(workspace.to_owned(), intent);
    }
}

/// Apply `intent` to the registry on a worker: it reads and writes a file.
/// The registry's watch then shows the result.
fn apply_aside(workspace: String, (choice, set_on, scope): WallpaperIntent) {
    let spawned = std::thread::Builder::new()
        .name("wallpaper-apply".into())
        .spawn(move || {
            let applied = crate::portal::desktop_services::apply_wallpaper_choice(
                &crate::portal::desktop_services::wallpaper_state_path(),
                &workspace,
                choice.as_deref(),
                &set_on,
                crate::portal::desktop_services::WallpaperScope::parse(Some(&scope)),
            );
            if let Err(error) = applied {
                tracing::warn!(%error, "could not apply where the wallpaper shows");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start applying where the wallpaper shows");
    }
}

fn current_choice() -> Option<String> {
    CHOICE.read().ok().and_then(|choice| choice.clone())
}

/// The wallpaper workspace `workspace_id` shows on this device, if any.
///
/// A person who has not chosen (no `appearance.wallpaper`) keeps a picture
/// of their own set before the setting existed; otherwise they see the
/// default shipped one.
pub fn native_wallpaper(workspace_id: &str) -> Option<ImageData> {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("workspace wallpaper lookup");
    if let Some(preview) =
        crate::portal::desktop_services::wallpaper_preview_file_for_workspace(workspace_id)
            .and_then(|path| preview_picture(&path))
    {
        return Some(preview);
    }
    let value = current_choice();
    match tilo_wallpapers::choice(value.as_deref()) {
        // A registry from before shipped pictures had records: the shipped
        // picture chosen then shows everywhere, as it did.
        Some(tilo_wallpapers::Choice::Bundled(wallpaper))
            if value.is_some() && !custom_wallpapers().places_shipped() =>
        {
            shipped_picture(wallpaper)
        }
        _ => wallpaper_image_for_workspace(workspace_id, "background").or_else(shipped_default),
    }
}

/// The shipped wallpaper the person chose, or the default one.
fn shipped_wallpaper() -> Option<ImageData> {
    match tilo_wallpapers::choice(current_choice().as_deref())? {
        tilo_wallpapers::Choice::Bundled(wallpaper) => shipped_picture(wallpaper),
        tilo_wallpapers::Choice::Custom => shipped_default(),
    }
}

fn shipped_default() -> Option<ImageData> {
    tilo_wallpapers::default_wallpaper().and_then(shipped_picture)
}

/// Decoded shipped pictures. Only the one on screen is kept (each is tens
/// of megabytes of pixels), with the one being decoded beside it.
#[derive(Default)]
struct Shipped {
    /// The picture last handed out, by wallpaper id.
    shown: Option<(&'static str, ImageData)>,
    /// A picture decoding off this thread, by wallpaper id.
    decoding: Option<&'static str>,
    /// Pictures that could not be read: the image does not carry them (a
    /// source checkout). Its `/usr` never changes under a session, so a
    /// miss is remembered rather than looked for again on every frame.
    missing: std::collections::HashSet<&'static str>,
    /// Bumped whenever `shown` changes, so a cached hue moves with it.
    generation: u64,
}

fn shipped() -> &'static Mutex<Shipped> {
    static SHIPPED: OnceLock<Mutex<Shipped>> = OnceLock::new();
    SHIPPED.get_or_init(|| Mutex::new(Shipped::default()))
}

fn decode_shipped(wallpaper: &tilo_wallpapers::Wallpaper) -> Option<ImageData> {
    let decoded = wallpaper.decode_picture()?;
    Some(ui::raster::rgba8(
        decoded.rgba,
        decoded.width,
        decoded.height,
    ))
}

fn show_shipped(state: &mut Shipped, id: &'static str, image: ImageData) {
    state.shown = Some((id, image));
    state.generation = state.generation.wrapping_add(1);
}

/// The picture of a shipped `wallpaper`, decoded once.
///
/// The first picture of a session is decoded on the spot, so the desktop
/// never shows anything else first. A different one later decodes on a
/// worker thread while the current picture stays up, and wakes the
/// compositor when it is ready: switching in Settings never stalls a frame.
/// A picture the image does not carry (a source checkout) shows nothing,
/// leaving the backdrop's gradient.
fn shipped_picture(wallpaper: &'static tilo_wallpapers::Wallpaper) -> Option<ImageData> {
    let id = wallpaper.id.as_str();
    let shown = {
        let state = shipped().lock().ok()?;
        match &state.shown {
            Some((shown, image)) if *shown == id => return Some(image.clone()),
            shown if state.missing.contains(id) => {
                return shown.as_ref().map(|(_, image)| image.clone());
            }
            shown => shown.as_ref().map(|(_, image)| image.clone()),
        }
    };
    match shown {
        Some(shown) => {
            decode_shipped_aside(wallpaper);
            Some(shown)
        }
        None => first_shipped_picture(wallpaper),
    }
}

/// Decode the session's first shipped picture on this thread.
fn first_shipped_picture(wallpaper: &'static tilo_wallpapers::Wallpaper) -> Option<ImageData> {
    let id = wallpaper.id.as_str();
    let image = decode_shipped(wallpaper);
    let mut state = shipped().lock().ok()?;
    if let Some(image) = image {
        show_shipped(&mut state, id, image.clone());
        Some(image)
    } else {
        state.missing.insert(id);
        None
    }
}

/// Decode `wallpaper` on a worker thread, once, and wake the compositor to
/// show it when it is ready.
fn decode_shipped_aside(wallpaper: &'static tilo_wallpapers::Wallpaper) {
    let id = wallpaper.id.as_str();
    {
        let Ok(mut state) = shipped().lock() else {
            return;
        };
        if state.decoding == Some(id) {
            return;
        }
        state.decoding = Some(id);
    }
    let spawned = std::thread::Builder::new()
        .name("wallpaper-decode".into())
        .spawn(move || {
            let image = decode_shipped(wallpaper);
            let Ok(mut state) = shipped().lock() else {
                return;
            };
            if state.decoding == Some(id) {
                state.decoding = None;
            }
            let Some(image) = image else {
                state.missing.insert(id);
                return;
            };
            show_shipped(&mut state, id, image);
            drop(state);
            crate::shell::wake_compositor();
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, wallpaper = id, "could not start decoding the wallpaper");
        if let Ok(mut state) = shipped().lock() {
            state.decoding = None;
        }
    }
}

/// The generation of the shipped picture on screen.
fn shipped_generation() -> u64 {
    shipped().lock().map_or(0, |state| state.generation)
}

/// What a workspace's wallpaper is, as far as its temperature is concerned:
/// the preview file, the registry's generation, and which shipped picture is
/// chosen and on screen. Read from memory only: the registry watch
/// ([`custom_wallpapers`]) bumps its generation when the file changes.
type WallpaperStamp = (Option<std::path::PathBuf>, u64, Option<String>, u64);

fn wallpaper_stamp(workspace_id: &str) -> WallpaperStamp {
    let preview =
        crate::portal::desktop_services::wallpaper_preview_file_for_workspace(workspace_id);
    (
        preview,
        custom_wallpapers().generation(),
        current_choice(),
        shipped_generation(),
    )
}

/// The neutral temperature workspace `workspace_id` takes on this device:
/// its wallpaper's dominant hue ([`ui::workspace_hue`]), or `None` for a
/// workspace whose wallpaper has none, which stays neutral. Decodes the
/// wallpaper only when it changed since the last call for that workspace.
pub fn native_workspace_hue(workspace_id: &str) -> Option<f32> {
    type KnownHues = HashMap<String, (WallpaperStamp, Option<f32>)>;
    static HUES: OnceLock<Mutex<KnownHues>> = OnceLock::new();
    let stamp = wallpaper_stamp(workspace_id);
    let hues = HUES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = hues.lock()
        && let Some((known, hue)) = cache.get(workspace_id)
        && *known == stamp
    {
        return *hue;
    }
    let hue = native_wallpaper(workspace_id).and_then(|image| ui::workspace_hue(image.data.data()));
    if let Ok(mut cache) = hues.lock() {
        cache.insert(workspace_id.to_owned(), (stamp, hue));
    }
    hue
}

/// The person's own wallpapers as the paint path sees them: the private
/// registry and the pictures it names, decoded. Paint only reads memory. A
/// watch on the registry's directory reloads it on a service thread when the
/// file changes, and pictures decode on worker threads that wake the
/// compositor when they are ready, while the picture already up stays up.
/// The registry is read once on the spot when the session first asks (and a
/// picture is decoded on the spot while nothing has been shown yet), so the
/// first frame shows the person's wallpaper rather than a stand-in.
struct CustomWallpapers {
    registry: std::path::PathBuf,
    cache_dir: std::path::PathBuf,
    state: Mutex<CustomState>,
    /// Bumped whenever the registry is reloaded or a picture lands.
    generation: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct CustomState {
    records: Vec<crate::portal::desktop_services::WallpaperRecord>,
    /// Decoded pictures by cache file name; files are named by the hash of
    /// their contents, so a name never comes to mean another picture.
    images: HashMap<String, ImageData>,
    /// Pictures decoding off the paint path, by cache file name.
    decoding: std::collections::HashSet<String>,
    /// Pictures that could not be read, until the registry changes.
    unreadable: std::collections::HashSet<String>,
    /// The picture last handed out per workspace and target, shown while
    /// the one replacing it decodes.
    shown: HashMap<(String, String), String>,
}

fn custom_wallpapers() -> &'static CustomWallpapers {
    static CUSTOM: OnceLock<CustomWallpapers> = OnceLock::new();
    let mut first = false;
    let custom = CUSTOM.get_or_init(|| {
        first = true;
        CustomWallpapers::new(crate::portal::desktop_services::wallpaper_state_path())
    });
    if first {
        custom.watch();
    }
    custom
}

impl CustomWallpapers {
    /// The registry at `registry`, read now.
    fn new(registry: std::path::PathBuf) -> Self {
        let cache_dir =
            crate::portal::desktop_services::wallpaper_cache_dir_for_renderer(&registry);
        let custom = Self {
            registry,
            cache_dir,
            state: Mutex::new(CustomState::default()),
            generation: std::sync::atomic::AtomicU64::new(0),
        };
        custom.reload();
        custom
    }

    fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    fn bump(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Read the registry again. Off the paint path except for the first read.
    fn reload(&self) {
        let records: Vec<crate::portal::desktop_services::WallpaperRecord> =
            std::fs::read(&self.registry)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.records == records && state.unreadable.is_empty() {
            return;
        }
        // Pictures neither named any more nor still on screen are let go
        // (one on screen stays up until its replacement has decoded).
        // Unreadable ones get another chance.
        let CustomState { images, shown, .. } = &mut *state;
        images.retain(|file, _| {
            records.iter().any(|record| &record.cache_file == file)
                || shown.values().any(|shown| shown == file)
        });
        state.unreadable.clear();
        state.records = records;
        drop(state);
        self.bump();
    }

    /// Reload the registry whenever its directory reports a change to it,
    /// on a service thread, and wake the compositor to draw what changed.
    fn watch(&'static self) {
        use notify::Watcher as _;
        let Some(directory) = self.registry.parent().map(std::path::Path::to_path_buf) else {
            return;
        };
        let Some(name) = self.registry.file_name().map(std::ffi::OsStr::to_os_string) else {
            return;
        };
        // The directory may not exist before the first wallpaper is set.
        let _ = std::fs::create_dir_all(&directory);
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut watcher) = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        }) else {
            tracing::warn!("wallpaper registry: no file watch; changes show after a restart");
            return;
        };
        if let Err(error) = watcher.watch(&directory, notify::RecursiveMode::NonRecursive) {
            tracing::warn!(%error, "wallpaper registry: could not watch its directory");
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("wallpaper-registry".into())
            .spawn(move || {
                // The watcher lives as long as the thread reading its events.
                let _watcher = watcher;
                while let Ok(event) = rx.recv() {
                    let touches_registry = event.as_ref().is_ok_and(|event| {
                        event
                            .paths
                            .iter()
                            .any(|path| path.file_name() == Some(name.as_os_str()))
                    });
                    if touches_registry || event.is_err() {
                        let before = self.generation();
                        self.reload();
                        if self.generation() != before {
                            crate::shell::wake_compositor();
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "wallpaper registry: could not start its watch");
        }
    }

    /// Whether the registry places shipped pictures too: written since
    /// "Show on" and "Use for" apply to the wallpaper the person has.
    fn places_shipped(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.records.iter().any(|record| record.shipped.is_some()))
    }

    /// The picture `workspace_id` shows as `target`, from memory.
    fn image_for(&'static self, workspace_id: &str, target: &str) -> Option<ImageData> {
        let picture = {
            let state = self.state.lock().ok()?;
            crate::portal::desktop_services::wallpaper_record_for(
                &state.records,
                workspace_id,
                target,
            )?
            .picture()
        };
        match picture {
            crate::portal::desktop_services::WallpaperPicture::Cached(file) => {
                self.picture(&file, Some((workspace_id, target)))
            }
            crate::portal::desktop_services::WallpaperPicture::Shipped(id) => {
                match tilo_wallpapers::choice(Some(&id))? {
                    tilo_wallpapers::Choice::Bundled(wallpaper) => shipped_picture(wallpaper),
                    tilo_wallpapers::Choice::Custom => None,
                }
            }
        }
    }

    /// The decoded picture in cache file `file`. One not decoded yet is
    /// decoded on a worker while the picture shown for `slot` before stays
    /// up; only the session's first picture is decoded on the spot.
    fn picture(&'static self, file: &str, slot: Option<(&str, &str)>) -> Option<ImageData> {
        if !safe_cache_name(file) {
            return None;
        }
        let mut state = self.state.lock().ok()?;
        if let Some(image) = state.images.get(file).cloned() {
            if let Some((workspace, target)) = slot {
                state
                    .shown
                    .insert((workspace.to_owned(), target.to_owned()), file.to_owned());
            }
            return Some(image);
        }
        if state.unreadable.contains(file) {
            return None;
        }
        let first = state.images.is_empty() && state.decoding.is_empty();
        if first {
            drop(state);
            let image = read_normalised_wallpaper(&self.cache_dir.join(file));
            let mut state = self.state.lock().ok()?;
            match &image {
                Some(image) => {
                    state.images.insert(file.to_owned(), image.clone());
                    if let Some((workspace, target)) = slot {
                        state
                            .shown
                            .insert((workspace.to_owned(), target.to_owned()), file.to_owned());
                    }
                }
                None => {
                    state.unreadable.insert(file.to_owned());
                }
            }
            drop(state);
            return image;
        }
        if state.decoding.insert(file.to_owned()) {
            self.decode_aside(file.to_owned());
        }
        let shown = slot
            .and_then(|(workspace, target)| {
                state.shown.get(&(workspace.to_owned(), target.to_owned()))
            })
            .and_then(|shown| state.images.get(shown))
            .cloned();
        drop(state);
        shown
    }

    fn decode_aside(&'static self, file: String) {
        let path = self.cache_dir.join(&file);
        let spawned = std::thread::Builder::new()
            .name("wallpaper-decode".into())
            .spawn(move || {
                let image = read_normalised_wallpaper(&path);
                let Ok(mut state) = self.state.lock() else {
                    return;
                };
                state.decoding.remove(&file);
                match image {
                    Some(image) => {
                        state.images.insert(file, image);
                    }
                    None => {
                        state.unreadable.insert(file);
                    }
                }
                drop(state);
                self.bump();
                crate::shell::wake_compositor();
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start decoding the wallpaper");
            if let Ok(mut state) = self.state.lock() {
                state.decoding.clear();
            }
        }
    }
}

/// Return a normalised, private wallpaper image for the named local workspace.
/// The cache only reads Tilo's 0600 registry and cache directory; it never
/// dereferences the original document URI. Paint reads memory only
/// (`CustomWallpapers`).
pub fn wallpaper_image_for_workspace(workspace_id: &str, target: &str) -> Option<ImageData> {
    custom_wallpapers().image_for(workspace_id, target)
}

/// The picture a wallpaper preview shows, by its cache file name.
fn preview_picture(path: &std::path::Path) -> Option<ImageData> {
    let file = path.file_name()?.to_str()?;
    custom_wallpapers().picture(file, None)
}

fn safe_cache_name(name: &str) -> bool {
    name.len() == 68
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        && name
            .as_bytes()
            .get(..64)
            .is_some_and(|hash| hash.iter().all(u8::is_ascii_hexdigit))
}

fn read_normalised_wallpaper(path: &std::path::Path) -> Option<ImageData> {
    let decoded = ui::png::read_rgba8(path).ok()?;
    Some(ui::raster::rgba8(
        decoded.rgba,
        decoded.width,
        decoded.height,
    ))
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: pixel coordinates and extents of a few thousand pixels"
)]
mod tests {
    use std::collections::HashMap;

    use taffy::prelude::TaffyMaxContent as _;
    use taffy::{NodeId, TaffyTree};
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use ui::widget::Widget;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect};

    use super::DesktopBackdrop;
    use crate::shell::overlay_host::OutputGeometry;

    /// The laptop panel at 2x and a 1080p monitor at 1x to its right, as
    /// the backend reports them: logical rectangles and scene pixels.
    fn panel_and_monitor() -> Vec<OutputGeometry> {
        let mut panel = OutputGeometry::unscaled(0.0, 0.0, 1472.0, 920.0).with_id(1);
        panel.scale = 2.0;
        panel.scene = Rect::new(0.0, 0.0, 2944.0, 1840.0);
        let mut monitor = OutputGeometry::unscaled(1472.0, 0.0, 1920.0, 1080.0).with_id(2);
        monitor.scene = Rect::new(2944.0, 0.0, 4864.0, 1080.0);
        vec![panel, monitor]
    }

    const QUADRANTS: [[u8; 3]; 4] = [[200, 40, 40], [40, 200, 40], [40, 40, 200], [220, 220, 40]];

    /// A 16:10 picture of four flat quadrants: top-left, top-right,
    /// bottom-left and bottom-right in [`QUADRANTS`]' order.
    fn quadrant_picture(width: u32, height: u32) -> vello::peniko::ImageData {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let index = usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2);
                rgba.extend_from_slice(&QUADRANTS[index]);
                rgba.push(255);
            }
        }
        ui::raster::rgba8(rgba, width, height)
    }

    /// Lay `backdrop` out over the logical extent of `outputs`, draw it the
    /// way the compositor does (logical scene, composed per display at its
    /// own scale) and return the scene's pixels.
    fn render(backdrop: &mut DesktopBackdrop, outputs: &[OutputGeometry]) -> (Vec<u8>, u32) {
        let (width, height) = crate::shell::output_scene::logical_extent(outputs);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = backdrop.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::style_helpers::length(width as f32),
                        height: taffy::style_helpers::length(height as f32),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        backdrop.apply_layout(&taffy, node, Point::ZERO);
        let mut logical = Scene::new();
        backdrop.draw(
            &mut logical,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::empty(),
        );
        let mut scene = Scene::new();
        crate::shell::output_scene::compose(outputs, &logical, &mut scene);
        let (scene_width, scene_height) = crate::shell::output_scene::scene_extent(outputs);
        let (scene_width, scene_height) = (scene_width as u32, scene_height as u32);
        let mut renderer = ui::offscreen::OffscreenRenderer::new(scene_width, scene_height);
        let pixels = renderer.render(&scene, vello::peniko::Color::BLACK);
        if let Some(dir) = std::env::var_os("TILO_WALLPAPER_SHOT_DIR") {
            let path = std::path::Path::new(&dir).join("backdrop-two-displays.png");
            renderer.render_png(&scene, vello::peniko::Color::BLACK, path);
        }
        (pixels, scene_width)
    }

    fn near(pixel: (u8, u8, u8, u8), want: [u8; 3]) -> bool {
        [pixel.0, pixel.1, pixel.2]
            .iter()
            .zip(want)
            .all(|(have, want)| have.abs_diff(want) <= 6)
    }

    /// Headless capture of the desktop's backdrop on a 2x laptop panel
    /// beside a 1x 1080p monitor: the wallpaper covers each display on its
    /// own (every display shows all four corners of the picture, cropped to
    /// its shape, rather than one picture stretched across both), the area
    /// outside the displays stays untouched, and a flat quadrant is flat:
    /// no grid, no noise, no tint over the picture.
    #[test]
    fn the_wallpaper_covers_each_display_at_its_own_scale() {
        let outputs = panel_and_monitor();
        let picture = quadrant_picture(2944, 1840);
        let mut backdrop = DesktopBackdrop::for_workspace("w")
            .with_wallpaper_provider(move |_| Some(picture.clone()));
        backdrop.set_displays(&outputs.iter().map(OutputGeometry::rect).collect::<Vec<_>>());
        let (pixels, width) = render(&mut backdrop, &outputs);
        let at = |x: u32, y: u32| ui::offscreen::OffscreenRenderer::sample(&pixels, width, x, y);
        // (scene rect, corners inset by 8 px) per display.
        for scene in [outputs[0].scene, outputs[1].scene] {
            let (x0, y0) = (scene.x0 as u32 + 8, scene.y0 as u32 + 8);
            let (x1, y1) = (scene.x1 as u32 - 9, scene.y1 as u32 - 9);
            for (corner, (x, y)) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
                .into_iter()
                .enumerate()
            {
                assert!(
                    near(at(x, y), QUADRANTS[corner]),
                    "display {scene:?}: corner {corner} at ({x},{y}) is {:?}, not {:?}",
                    at(x, y),
                    QUADRANTS[corner]
                );
            }
        }
        // Below the 1080-pixel monitor nothing is drawn.
        assert!(near(at(3900, 1500), [0, 0, 0]), "{:?}", at(3900, 1500));
        // A whole row and column through the panel's top-left quadrant, one
        // scene pixel apart: a grid line, noise or a gradient would show.
        for x in 0..1400 {
            assert!(
                near(at(x, 300), QUADRANTS[0]),
                "panel ({x},300) is {:?}",
                at(x, 300)
            );
        }
        for y in 0..880 {
            assert!(
                near(at(700, y), QUADRANTS[0]),
                "panel (700,{y}) is {:?}",
                at(700, y)
            );
        }
        for x in 2944..3880 {
            assert!(
                near(at(x, 300), QUADRANTS[0]),
                "monitor ({x},300) is {:?}",
                at(x, 300)
            );
        }
    }

    /// A backdrop asks for a frame when its host's wallpaper changes (a
    /// choice in Settings, a portal import), and not otherwise.
    #[test]
    fn a_changed_wallpaper_asks_for_a_frame() {
        let first = quadrant_picture(16, 10);
        let second = quadrant_picture(16, 10);
        let shown = std::rc::Rc::new(std::cell::RefCell::new(first));
        let source = shown.clone();
        let mut backdrop = DesktopBackdrop::for_workspace("w")
            .with_wallpaper_provider(move |_| Some(source.borrow().clone()));
        let outputs = vec![OutputGeometry::unscaled(0.0, 0.0, 64.0, 40.0)];
        assert!(backdrop.needs_redraw());
        let _ = render(&mut backdrop, &outputs);
        assert!(!backdrop.needs_redraw());
        *shown.borrow_mut() = second;
        assert!(backdrop.needs_redraw());
    }

    /// A custom wallpaper registry in a scratch directory, with one picture
    /// per name in its cache, as the portal writes them.
    fn scratch_registry(
        pictures: &[(&str, [u8; 3])],
    ) -> (tempfile::TempDir, std::path::PathBuf, Vec<String>) {
        let dir = tempfile::tempdir().expect("scratch directory");
        let registry = dir.path().join("portal").join("wallpapers.json");
        let cache = dir.path().join("portal").join("wallpaper-cache");
        std::fs::create_dir_all(&cache).expect("cache directory");
        let names = pictures
            .iter()
            .enumerate()
            .map(|(index, (_, colour))| {
                let name = format!("{index:064x}.png");
                let rgba: Vec<u8> = std::iter::repeat_n([colour[0], colour[1], colour[2], 255], 16)
                    .flatten()
                    .collect();
                let png = ui::png::encode_rgba8(4, 4, &rgba).expect("encode");
                std::fs::write(cache.join(&name), png).expect("picture");
                name
            })
            .collect();
        (dir, registry, names)
    }

    fn write_registry(registry: &std::path::Path, file: &str) {
        let records = vec![crate::portal::desktop_services::WallpaperRecord {
            workspace_id: "w".into(),
            cache_file: file.into(),
            target: "background".into(),
            shipped: None,
        }];
        let staged = registry.with_extension("partial");
        std::fs::write(&staged, serde_json::to_vec(&records).expect("json")).expect("registry");
        std::fs::rename(&staged, registry).expect("registry in place");
    }

    fn first_pixel(image: &vello::peniko::ImageData) -> [u8; 3] {
        let data = image.data.data();
        [data[0], data[1], data[2]]
    }

    /// The paint path reads the registry from memory: once loaded, a lookup
    /// touches no file, so a busy disk can never stall a frame. A changed
    /// registry arrives through the watch, and its picture decodes off the
    /// paint path while the one shown stays up.
    #[test]
    fn custom_wallpapers_reach_paint_through_the_watch_not_through_stat() {
        let (dir, registry, names) =
            scratch_registry(&[("red", [200, 0, 0]), ("blue", [0, 0, 200])]);
        write_registry(&registry, &names[0]);
        let custom: &'static super::CustomWallpapers =
            Box::leak(Box::new(super::CustomWallpapers::new(registry.clone())));
        let shown = custom.image_for("w", "background").expect("first picture");
        assert_eq!(first_pixel(&shown), [200, 0, 0]);

        // No file read on a lookup: the registry and the picture can go.
        let parked = dir.path().join("parked.json");
        std::fs::rename(&registry, &parked).expect("move the registry away");
        std::fs::remove_file(registry.with_file_name("wallpaper-cache").join(&names[0]))
            .expect("remove the picture");
        let again = custom.image_for("w", "background").expect("still shown");
        assert_eq!(first_pixel(&again), [200, 0, 0]);
        std::fs::rename(&parked, &registry).expect("registry back");

        custom.watch();
        let before = custom.generation();
        write_registry(&registry, &names[1]);
        tilo_test_env::wait_until("the watch to deliver the new wallpaper", || {
            if custom.generation() != before
                && let Some(image) = custom.image_for("w", "background")
                && first_pixel(&image) == [0, 0, 200]
            {
                return true;
            }
            // The picture shown meanwhile is the old one, never nothing.
            if let Some(image) = custom.image_for("w", "background") {
                assert!(
                    [[200, 0, 0], [0, 0, 200]].contains(&first_pixel(&image)),
                    "only the old or the new picture is ever shown"
                );
            }
            false
        });
    }
}
