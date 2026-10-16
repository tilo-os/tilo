//! The desktop backdrop with this device's wallpapers.
//!
//! Drawing is the shared [`DesktopBackdrop`]; this module supplies the
//! wallpaper each workspace shows. The person's `appearance.wallpaper`
//! setting chooses it ([`tilo_wallpapers::choice`]): one of the pictures the
//! image ships, decoded once and kept, or their own picture, read from
//! Tilo's own 0600 registry and cache (the portal's preview file first).
//! A custom wallpaper is per device.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};
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

/// Take the `appearance.wallpaper` value from the settings `document`.
/// Called whenever the followed settings change; cheap when the value did
/// not, and the picture it names is decoded only when it is drawn.
pub fn follow_wallpaper_choice(document: &serde_json::Map<String, serde_json::Value>) {
    let value = settings_store::appearance::WALLPAPER.from_document(document);
    if let Ok(mut choice) = CHOICE.write()
        && *choice != value
    {
        *choice = value;
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
            .and_then(|path| read_normalised_wallpaper(&path))
    {
        return Some(preview);
    }
    let value = current_choice();
    match tilo_wallpapers::choice(value.as_deref()) {
        Some(tilo_wallpapers::Choice::Bundled(wallpaper)) if value.is_some() => {
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
/// the preview file and its age, the age of the wallpaper registry, and
/// which shipped picture is chosen and on screen.
/// Cheap to read every few seconds; the image is decoded only when it moves.
type WallpaperStamp = (
    Option<std::path::PathBuf>,
    Option<SystemTime>,
    Option<SystemTime>,
    Option<String>,
    u64,
);

fn wallpaper_stamp(workspace_id: &str) -> WallpaperStamp {
    let modified = |path: &std::path::Path| std::fs::metadata(path).ok()?.modified().ok();
    let preview =
        crate::portal::desktop_services::wallpaper_preview_file_for_workspace(workspace_id);
    let preview_modified = preview.as_deref().and_then(modified);
    let registry_modified = modified(&crate::portal::desktop_services::wallpaper_state_path());
    (
        preview,
        preview_modified,
        registry_modified,
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

#[derive(Default)]
struct WallpaperRenderCache {
    checked: Option<Instant>,
    stamp: Option<SystemTime>,
    records: Vec<crate::portal::desktop_services::WallpaperRecord>,
    images: HashMap<String, ImageData>,
}

fn wallpaper_render_cache() -> &'static Mutex<WallpaperRenderCache> {
    static CACHE: OnceLock<Mutex<WallpaperRenderCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(WallpaperRenderCache::default()))
}

/// Return a normalised, private wallpaper image for the named local workspace.
/// The cache only reads Tilo's 0600 registry and cache directory; it never
/// dereferences the original document URI.
pub fn wallpaper_image_for_workspace(workspace_id: &str, target: &str) -> Option<ImageData> {
    let path = crate::portal::desktop_services::wallpaper_state_path();
    let cache_dir = crate::portal::desktop_services::wallpaper_cache_dir_for_renderer(&path);
    let mut cache = wallpaper_render_cache().lock().ok()?;
    let now = Instant::now();
    if cache
        .checked
        .is_none_or(|checked| now.duration_since(checked) >= Duration::from_millis(250))
    {
        cache.checked = Some(now);
        let stamp = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        if stamp != cache.stamp {
            cache.stamp = stamp;
            cache.records = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            cache.images.clear();
        }
    }
    let file = crate::portal::desktop_services::wallpaper_record_for(
        &cache.records,
        workspace_id,
        target,
    )?
    .cache_file
    .clone();
    if !safe_cache_name(&file) {
        return None;
    }
    if !cache.images.contains_key(&file) {
        let image = read_normalised_wallpaper(&cache_dir.join(&file))?;
        cache.images.insert(file.clone(), image);
    }
    cache.images.get(&file).cloned()
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
}
