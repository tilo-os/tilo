//! The pictures a page shows of the driven app's widgets it shows as their paint
//! ([`crate::composition::page::shows_paint`]): what a host that renders the app's composition
//! with elements of its own ([`Driver::composition`]) shows for a chart's marks or a drawing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine as _;
use taffy::NodeId;
use vello::Scene;
use vello::kurbo::Affine;

use super::Driver;
use crate::composition::{Composed, Composition};

/// Device pixels a page's picture has for each logical pixel of its widget: a sharp picture on a
/// screen of twice the density, scaled down on any other.
const PICTURE_SCALE: f64 = 2.0;

/// The pictures last handed to a composition, by the path of their widget, each with the version
/// of the paint it shows.
#[derive(Default)]
pub(super) struct Pictures(HashMap<String, (u64, Arc<str>)>);

impl Driver {
    /// `composition`, read from this frame ([`Self::composition`]), with the picture of each
    /// widget a page shows as its paint ([`crate::composition::page::shows_paint`]): its paint
    /// alone, drawn at twice its size for a sharp screen. A picture is drawn again only when
    /// what the widget paints changed.
    ///
    /// A part the walk took for its paint alone, because it says nothing else of itself, is no
    /// picture where its paint is nothing: the page shows what the part says, or nothing, and is
    /// never handed an image of nothing. A part the window shows as a picture of its own paint — a
    /// control still in one state, hidden from assistive technology — stays one whatever it paints.
    pub fn paint_pictures(&mut self, composition: &mut Composition) {
        let mut wanted = Vec::new();
        for node in std::iter::once(&composition.root).chain(&composition.layers) {
            painted_parts(node, &mut wanted);
        }
        let mut drawn = std::mem::take(&mut self.pictures.0);
        let mut targets = None;
        let mut shown = HashMap::with_capacity(wanted.len());
        // What paints nothing, by the path of its part: a region that holds no child to draw, a
        // control resting between its own paints.
        let mut blank = HashSet::new();
        for (path, rect) in wanted {
            let Some((scene, [_, _, width, height], of_its_own)) =
                self.scene_of(&path, rect, &mut targets)
            else {
                blank.insert(path);
                continue;
            };
            if scene.encoding().is_empty() {
                if !of_its_own {
                    blank.insert(path);
                }
                continue;
            }
            let version = scene_version(&scene, width, height);
            let kept = drawn.remove(&path).filter(|(was, _)| *was == version);
            let picture = match kept {
                Some((_, picture)) => Some(picture),
                None => picture_of(&path, &scene, width, height),
            };
            if let Some(picture) = picture {
                shown.insert(path, (version, picture));
            }
        }
        for node in std::iter::once(&mut composition.root).chain(&mut composition.layers) {
            hand_pictures(node, &shown, &blank);
        }
        self.pictures.0 = shown;
    }

    /// The paint of the widget at `path`, with its box's top left at the origin, at the picture's
    /// scale, and whether the window hides it from assistive technology: what it shows is then a
    /// picture of itself, painted or not. None for a path no laid-out widget has.
    fn scene_of(
        &mut self,
        path: &str,
        rect: [f64; 4],
        targets: &mut Option<HashMap<String, NodeId>>,
    ) -> Option<(Scene, [f64; 4], bool)> {
        let node = *targets
            .get_or_insert_with(|| self.named_target_nodes())
            .get(path)?;
        let app = &mut *self.app;
        let text = &mut app.text_renderer;
        let theme = &app.theme;
        let presence = &app.presence_ctx;
        // SAFETY: the driver owns the app's laid-out tree and holds it exclusively here; no
        // reference into it outlives the call.
        unsafe {
            crate::bound_dispatch::mutate_bound_node(
                &app.bindings,
                &app.layout.node_map,
                node,
                |widget| {
                    let of_its_own = widget.accessibility_hidden();
                    let mut scene = Scene::new();
                    let transform =
                        Affine::scale(PICTURE_SCALE) * Affine::translate((-rect[0], -rect[1]));
                    crate::widget::DrawChild::draw_child(
                        widget,
                        &mut scene,
                        transform,
                        text,
                        theme,
                        &presence.draw_env(),
                    );
                    (scene, rect, of_its_own)
                },
            )
        }
        .ok()
    }
}

/// The picture of `scene`, `width` by `height` logical pixels, as a PNG's `data:` address.
fn picture_of(path: &str, scene: &Scene, width: f64, height: f64) -> Option<Arc<str>> {
    let pixels = |logical: f64| {
        let device = (logical * PICTURE_SCALE).ceil();
        (1.0..=f64::from(u16::MAX))
            .contains(&device)
            .then(|| crate::num::u32_of_f64(device))
    };
    let (width, height) = (pixels(width)?, pixels(height)?);
    let rgba =
        crate::offscreen::render_picture(scene, width, height, vello::peniko::Color::TRANSPARENT)
            .inspect_err(|why| tracing::warn!(path, %why, "a page's picture could not be drawn"))
            .ok()?;
    let png = crate::png::encode_rgba8(width, height, &rgba).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    Some(format!("data:image/png;base64,{encoded}").into())
}

/// What `scene` draws, as a number that changes when it does: its paths, its draws and their
/// colours, its glyphs and where they stand, and its size.
fn scene_version(scene: &Scene, width: f64, height: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let encoding = scene.encoding();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    width.to_bits().hash(&mut hash);
    height.to_bits().hash(&mut hash);
    encoding.path_data.hash(&mut hash);
    encoding.draw_data.hash(&mut hash);
    encoding.path_tags.len().hash(&mut hash);
    encoding.draw_tags.len().hash(&mut hash);
    encoding.transforms.len().hash(&mut hash);
    encoding.resources.patches.len().hash(&mut hash);
    for glyph in &encoding.resources.glyphs {
        glyph.id.hash(&mut hash);
        glyph.x.to_bits().hash(&mut hash);
        glyph.y.to_bits().hash(&mut hash);
    }
    hash.finish()
}

/// Every widget under `node` that shows as a picture, with where it stands.
fn painted_parts(node: &Composed, out: &mut Vec<(String, [f64; 4])>) {
    if node.picture.is_some() {
        out.push((node.path.clone(), node.rect));
    }
    for child in &node.children {
        painted_parts(child, out);
    }
}

/// `node` with each picture of `shown` handed to the widget it is of, and each part `blank` says
/// paints nothing no longer a picture.
fn hand_pictures(
    node: &mut Composed,
    shown: &HashMap<String, (u64, Arc<str>)>,
    blank: &HashSet<String>,
) {
    if blank.contains(&node.path) {
        node.picture = None;
    } else if let Some(picture) = &mut node.picture {
        match shown.get(&node.path) {
            Some((version, drawn)) => {
                picture.version = *version;
                picture.data = Some(Arc::clone(drawn));
            }
            None => picture.data = None,
        }
    }
    for child in &mut node.children {
        hand_pictures(child, shown, blank);
    }
}
