//! The scene of one output: what is drawn where, front first.
//! Everything on screen is a buffer: client surfaces (their popups and subsurfaces included) and
//! the layers of hosted surfaces. The scene is renderer-generic, so the same elements feed the CPU
//! renderer of a headless output and the GPU of a real one, and their damage tracking decides what
//! is redrawn.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use std::time::Instant;

use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{AsRenderElements, Element, Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{ImportAll, ImportDma, ImportMem, Renderer};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size};

use crate::a11y::{self, MagnifierView};
use crate::effects::WindowEffect;
use crate::state::State;
use crate::uploads::{Placed, Uploads};

use tilo_session_core::surface::HostLayer;

/// The colour behind every window: the desktop's background before a wallpaper surface covers it.
pub const BACKGROUND: [f32; 4] = [0.102, 0.106, 0.149, 1.0];

smithay::backend::renderer::element::render_elements! {
    /// One drawable thing on an output.
    pub OutputRenderElement<R> where R: ImportAll + ImportMem;
    /// A client surface.
    Surface=WaylandSurfaceRenderElement<R>,
    /// A hosted surface's layer drawn on the CPU, or a client surface's HDR rendition where the
    /// surface itself would be.
    Memory=MemoryRenderBufferRenderElement<R>,
    /// A hosted surface's layer drawn on the GPU, shown from the dma-buf it was drawn into.
    Hosted=TextureRenderElement<R::TextureId>,
    /// A hosted surface's layer drawn on the GPU, under the magnifier.
    ZoomedGpuHosted=RescaleRenderElement<TextureRenderElement<R::TextureId>>,
    /// A hosted surface's layer drawn on the GPU, inside the magnifier's lens.
    LensHosted=CropRenderElement<RescaleRenderElement<TextureRenderElement<R::TextureId>>>,
    /// The idle dim over everything, where no backlight dims.
    Dim=SolidColorRenderElement,
    /// A client surface drawn scaled: under the magnifier, or as the lock screen filling a display
    /// it was not made for.
    ScaledSurface=RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    /// A hosted surface's layer, or a client surface's HDR rendition, under the magnifier.
    ZoomedHosted=RescaleRenderElement<MemoryRenderBufferRenderElement<R>>,
    /// A client surface inside the magnifier's lens.
    LensSurface=CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<R>>>,
    /// A window under the whole-screen magnifier.
    ZoomedTransformed=RescaleRenderElement<crate::effects::TransformedElement<R>>,
    /// A window's shadow and edge under the whole-screen magnifier.
    ZoomedFrame=RescaleRenderElement<crate::effects::FrameElement<R>>,
    /// A closed window's last frame under the whole-screen magnifier.
    ZoomedSnapshot=RescaleRenderElement<crate::effects::SnapshotElement<R::TextureId>>,
    /// A solid area under the whole-screen magnifier.
    ZoomedDim=RescaleRenderElement<SolidColorRenderElement>,
    /// A window inside the magnifier's lens.
    LensTransformed=CropRenderElement<RescaleRenderElement<crate::effects::TransformedElement<R>>>,
    /// A window's shadow and edge inside the magnifier's lens.
    LensFrame=CropRenderElement<RescaleRenderElement<crate::effects::FrameElement<R>>>,
    /// A closed window's last frame inside the magnifier's lens.
    LensSnapshot=CropRenderElement<RescaleRenderElement<crate::effects::SnapshotElement<R::TextureId>>>,
    /// A solid area inside the magnifier's lens.
    LensDim=CropRenderElement<RescaleRenderElement<SolidColorRenderElement>>,
    /// A window drawn moved, scaled or faded, or pictured by a hosted surface.
    Transformed=crate::effects::TransformedElement<R>,
    /// A closed window's last frame, drawn moved, scaled or faded.
    Snapshot=crate::effects::SnapshotElement<R::TextureId>,
    /// A window's shadow and edge, behind it.
    Frame=crate::effects::FrameElement<R>,
}

/// `element` scaled by `zoom` about `centre` , which stays where it is on screen.
fn scaled<E>(element: E, centre: Point<i32, Physical>, zoom: f64) -> RescaleRenderElement<E>
where
    E: Element,
{
    RescaleRenderElement::from_element(element, centre, zoom)
}

/// `element` scaled by `zoom` about `centre` , kept inside `crop` (output physical pixels): what
/// the lens shows of it, or nothing where the crop misses it.
fn cropped<E>(
    element: E,
    centre: Point<i32, Physical>,
    zoom: f64,
    crop: Rectangle<i32, Physical>,
    scale: Scale<f64>,
) -> Option<CropRenderElement<RescaleRenderElement<E>>>
where
    E: Element,
{
    CropRenderElement::from_element(scaled(element, centre, zoom), scale, crop)
}

/// `element` as the whole-screen magnifier shows it: scaled by `zoom` about `centre` , which stays
/// where it is on screen.
fn zoomed<R>(
    element: OutputRenderElement<R>,
    centre: Point<i32, Physical>,
    zoom: f64,
) -> OutputRenderElement<R>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    match element {
        OutputRenderElement::Surface(e) => {
            OutputRenderElement::ScaledSurface(scaled(e, centre, zoom))
        }
        OutputRenderElement::Memory(e) => {
            OutputRenderElement::ZoomedHosted(scaled(e, centre, zoom))
        }
        OutputRenderElement::Hosted(e) => {
            OutputRenderElement::ZoomedGpuHosted(scaled(e, centre, zoom))
        }
        OutputRenderElement::Dim(e) => OutputRenderElement::ZoomedDim(scaled(e, centre, zoom)),
        OutputRenderElement::Transformed(e) => {
            OutputRenderElement::ZoomedTransformed(scaled(e, centre, zoom))
        }
        OutputRenderElement::Snapshot(e) => {
            OutputRenderElement::ZoomedSnapshot(scaled(e, centre, zoom))
        }
        OutputRenderElement::Frame(e) => OutputRenderElement::ZoomedFrame(scaled(e, centre, zoom)),
        other => other,
    }
}

/// `element` as the lens shows it: the same magnified copy as [`zoomed`],
/// kept inside `crop` (output physical pixels). Elements with nothing to
/// show there are dropped.
fn lensed<R>(
    element: OutputRenderElement<R>,
    centre: Point<i32, Physical>,
    zoom: f64,
    crop: Rectangle<i32, Physical>,
    scale: Scale<f64>,
) -> Option<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    match element {
        OutputRenderElement::Surface(e) => Some(OutputRenderElement::LensSurface(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        // A memory buffer and a window's frame are drawn the same way: a crop of a scaled copy.
        OutputRenderElement::Memory(e) => Some(OutputRenderElement::Frame(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        OutputRenderElement::Hosted(e) => Some(OutputRenderElement::LensHosted(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        OutputRenderElement::Frame(e) => Some(OutputRenderElement::LensFrame(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        OutputRenderElement::Transformed(e) => Some(OutputRenderElement::LensTransformed(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        OutputRenderElement::Snapshot(e) => Some(OutputRenderElement::LensSnapshot(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        OutputRenderElement::Dim(e) => Some(OutputRenderElement::LensDim(cropped(
            e, centre, zoom, crop, scale,
        )?)),
        _ => None,
    }
}

/// A solid rectangle in output physical pixels, drawn once: the lens's backing and the visual
/// alert.
fn solid(at: Rectangle<i32, Physical>, color: Color32F) -> SolidColorRenderElement {
    SolidColorRenderElement::new(
        Id::new(),
        at,
        CommitCounter::from(0),
        color,
        Kind::Unspecified,
    )
}

/// The crosshair's guides through `pointer` (output physical pixels): a full-length line on each
/// axis, dark under light so it shows on any background.
fn crosshair_elements(
    geometry: Rectangle<i32, Physical>,
    pointer: Point<i32, Physical>,
) -> [SolidColorRenderElement; 4] {
    /// A half-transparent ink that reads on light and dark backgrounds under a brighter core.
    const INK: Color32F = Color32F::new(0.0, 0.0, 0.0, 0.75);
    const CORE: Color32F = Color32F::new(1.0, 1.0, 1.0, 0.9);
    let (w, h) = (geometry.size.w, geometry.size.h);
    let (x, y) = (pointer.x, pointer.y);
    let band = |x0: i32, y0: i32, x1: i32, y1: i32| {
        Rectangle::new(
            Point::from((x0, y0)),
            Size::from(((x1 - x0).max(0), (y1 - y0).max(0))),
        )
    };
    [
        solid(band(geometry.loc.x, y, geometry.loc.x + w, y + 1), CORE),
        solid(band(x, geometry.loc.y, x + 1, geometry.loc.y + h), CORE),
        solid(band(geometry.loc.x, y - 1, geometry.loc.x + w, y + 2), INK),
        solid(band(x - 1, geometry.loc.y, x + 2, geometry.loc.y + h), INK),
    ]
}

/// The idle dim's buffer of one output.
struct DimBuffer(std::cell::RefCell<SolidColorBuffer>);

/// The dim over all of `output` , when the scene is dimmed without a backlight.
fn dim_element<R>(state: &State, output: &Output) -> Option<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let alpha = state.dim_alpha()?;
    let geometry = state.space.output_geometry(output)?;
    output
        .user_data()
        .insert_if_missing(|| DimBuffer(std::cell::RefCell::new(SolidColorBuffer::default())));
    let buffer = output.user_data().get::<DimBuffer>()?;
    let mut buffer = buffer.0.borrow_mut();
    buffer.update(geometry.size, Color32F::new(0.0, 0.0, 0.0, alpha));
    let scale = Scale::from(output.current_scale().fractional_scale());
    Some(OutputRenderElement::Dim(
        SolidColorRenderElement::from_buffer(
            &buffer,
            Point::<i32, Physical>::from((0, 0)),
            scale,
            1.0,
            Kind::Unspecified,
        ),
    ))
}

/// A hosted surface's layer, drawn at `scale` with the layer's top left corner at `location`
/// (physical): the part of the frame it shows, at the pixel of the output the layer covers, so a
/// layer the size of a display that paints a bar costs its bar rather than the display. `None` when
/// the layer shows nothing there is to draw.
fn hosted_layer<R>(
    renderer: &mut R,
    uploads: &Uploads,
    layer: &HostLayer,
    location: Point<f64, Physical>,
    scale: f64,
) -> Result<Option<OutputRenderElement<R>>, R::Error>
where
    R: Renderer + ImportAll + ImportMem + ImportDma,
    R::TextureId: Send + Clone + 'static,
{
    let (width, height) = layer.frame.size();
    let Some(shown) = layer
        .frame
        .shown()
        .filter(|shown| shown.width > 0 && shown.height > 0 && shown.x < width && shown.y < height)
    else {
        return Ok(None);
    };
    let (width, height) = (f64::from(width), f64::from(height));
    // The frame scaled to the rectangle the layer covers, so a layer drawn at the output's scale is
    // shown at the pixels it was drawn for.
    let across = f64::from(layer.size.0) / width;
    let down = f64::from(layer.size.1) / height;
    let src = Rectangle::new(
        Point::from((f64::from(shown.x), f64::from(shown.y))),
        Size::from((f64::from(shown.width), f64::from(shown.height))),
    );
    let drawn = Size::<f64, Logical>::from((
        f64::from(shown.width) * across,
        f64::from(shown.height) * down,
    ))
    .to_i32_round::<i32>();
    let dst = Size::from((drawn.w.max(1), drawn.h.max(1)));
    let at = location
        + Point::from((f64::from(shown.x) * across, f64::from(shown.y) * down)).to_physical(scale);
    uploads.element(
        renderer,
        &layer.frame,
        Placed {
            at,
            src: Some(src),
            size: Some(dst),
        },
    )
}

/// What lies below hosted surface `host` , front first: the hosts beneath it and the windows, drawn
/// at `scale` with `origin` (logical) at the top left of the area `bounds` (physical), as its
/// backdrop sees them — the windows where their effects draw them, framed as they are drawn.
pub(crate) fn below_host<R>(
    renderer: &mut R,
    state: &State,
    host: usize,
    origin: Point<f64, Logical>,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem + ImportDma,
    R::TextureId: Clone + Send + 'static,
{
    let mut elements = Vec::new();
    if state.locked() {
        return elements;
    }
    for below in state.hosts.iter().take(host).rev() {
        for layer in below.layers().into_iter().rev() {
            let location =
                (Point::<i32, Logical>::from(layer.location).to_f64() - origin).to_physical(scale);
            match hosted_layer(renderer, &state.uploads, &layer, location, scale) {
                Ok(Some(element)) => elements.push(element),
                Ok(None) => {}
                Err(error) => tracing::warn!(?error, "hosted layer: buffer not imported"),
            }
        }
    }
    elements.extend(window_elements(
        renderer,
        state,
        origin,
        scale,
        bounds,
        state.space.elements().rev().cloned().collect(),
    ));
    elements
}

/// The window layer, front first: `stack` (the mapped windows, bottom first, reversed by the
/// caller) and the windows that closed and are still drawn where they stood, each drawn at `scale`
/// with `origin` at the top left of the area `bounds` .
fn window_elements<R>(
    renderer: &mut R,
    state: &State,
    origin: Point<f64, Logical>,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
    stack: Vec<Window>,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    // A window that closed keeps the place in the stack it had, so an animation of it runs where it
    // was drawn, not over the windows above.
    let len = stack.len();
    let mut closing: Vec<Vec<&crate::state::Closing>> = vec![Vec::new(); len + 1];
    for gone in &state.closing {
        if let Some(behind) = closing.get_mut(len.saturating_sub(gone.index).min(len)) {
            behind.push(gone);
        }
    }
    let mut elements = Vec::new();
    // What the desks draw over the windows: one a window in front of another stands on lies over it,
    // so everything behind is drawn around it.
    let mut covers: Vec<Rectangle<i32, Physical>> = Vec::new();
    for (front, window) in stack.into_iter().enumerate() {
        let window = &window;
        for gone in closing.get(front).into_iter().flatten() {
            elements.extend(closing_elements(
                renderer, state, gone, origin, scale, bounds,
            ));
        }
        let Some(location) = state.space.element_location(window) else {
            continue;
        };
        let effect = state.effect_of(window);
        let framing = state.framing(window);
        // What this window still shows through: what the desks cover it with for the windows in
        // front of it.
        let visible = outside(bounds, &covers);
        // Its own cover is over the windows behind it, from here on.
        if let Some(cover) = crate::state::describe(state, window)
            .and_then(|info| state.covers.get(&info.id).copied())
        {
            covers.push(Rectangle::new(
                cover.loc.to_physical_precise_round(scale),
                cover.size.to_physical_precise_round(scale),
            ));
        }
        // What the window drew is kept while it stands, so its close has a frame to animate even
        // once its surface is gone.
        state.record_snapshot(renderer, window);
        // A window drawn as it stands keeps its buffer's own element, which a display can scan out;
        // anything else is drawn through its frame. A window a cover cuts into keeps the corners it
        // has and draws the rest of itself.
        let whole = effect == WindowEffect::default() && framing.frame.is_square();
        if !whole || visible.len() != 1 || visible.first() != Some(&bounds) {
            for piece in &visible {
                elements.extend(crate::effects::with_effect(
                    renderer,
                    framing,
                    crate::effects::Content::Window(window),
                    location.to_f64() - origin,
                    &effect,
                    (*piece, scale),
                ));
            }
            continue;
        }
        let at = location.to_f64() - window.geometry().loc.to_f64() - origin;
        let hdr = crate::hdr::pieces(
            renderer,
            state,
            window,
            at.to_physical_precise_round(scale),
            scale,
            1.0,
        );
        match hdr {
            Some(pieces) => {
                elements.extend(pieces.into_iter().map(crate::hdr::Piece::into_output));
            }
            None => elements.extend(window.render_elements::<OutputRenderElement<R>>(
                renderer,
                at.to_physical_precise_round(scale),
                Scale::from(scale),
                1.0,
            )),
        }
    }
    for gone in closing.get(len).into_iter().flatten() {
        elements.extend(closing_elements(
            renderer, state, gone, origin, scale, bounds,
        ));
    }
    elements
}

/// `bounds` with every one of `covers` taken out of it: the pieces it still shows in.
fn outside(
    bounds: Rectangle<i32, Physical>,
    covers: &[Rectangle<i32, Physical>],
) -> Vec<Rectangle<i32, Physical>> {
    let mut pieces = vec![bounds];
    for cover in covers {
        let Some(cover) = cover.intersection(bounds) else {
            continue;
        };
        let mut left = Vec::with_capacity(pieces.len().saturating_mul(4));
        for piece in pieces {
            let Some(overlap) = piece.intersection(cover) else {
                left.push(piece);
                continue;
            };
            let (x0, y0) = (piece.loc.x, piece.loc.y);
            let (x1, y1) = (x0 + piece.size.w, y0 + piece.size.h);
            let (cx0, cy0) = (overlap.loc.x, overlap.loc.y);
            let (cx1, cy1) = (cx0 + overlap.size.w, cy0 + overlap.size.h);
            for (ax0, ay0, ax1, ay1) in [
                (x0, y0, x1, cy0),
                (x0, cy1, x1, y1),
                (x0, cy0, cx0, cy1),
                (cx1, cy0, x1, cy1),
            ] {
                if ax1 > ax0 && ay1 > ay0 {
                    left.push(Rectangle::new(
                        Point::from((ax0, ay0)),
                        Size::from((ax1 - ax0, ay1 - ay0)),
                    ));
                }
            }
        }
        pieces = left;
    }
    pieces
}

/// A window that closed, drawn from its last buffer with whatever effect the desktop is animating
/// it away with.
fn closing_elements<R>(
    renderer: &mut R,
    state: &State,
    closing: &crate::state::Closing,
    origin: Point<f64, Logical>,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let effect = state
        .effects
        .get(&closing.info.id)
        .copied()
        .unwrap_or_default();
    let framing = crate::effects::Framing {
        frame: closing.frame,
        images: &state.frame_images,
        snapshots: &state.snapshots,
        hdr: &state.hdr,
        state,
    };
    // The frame the window last drew, kept before its surface went: the desktop animates that away.
    let Some(snapshot) = closing.snapshot.as_ref() else {
        return Vec::new();
    };
    crate::effects::with_effect(
        renderer,
        framing,
        crate::effects::Content::Snapshot(snapshot),
        closing.rect.loc.to_f64() - origin,
        &effect,
        (bounds, scale),
    )
}

/// The scene `output` shows, front first, in its physical pixels: the desk,
/// the windows and the layers above and below them.
fn scene_elements<R>(
    renderer: &mut R,
    state: &State,
    output: &Output,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem + ImportDma,
    R::TextureId: Clone + Send + 'static,
{
    let Some(geometry) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let dim = dim_element(state, output);
    let output_scale = output.current_scale().fractional_scale();
    let scale = Scale::from(output_scale);
    let bounds = Rectangle::from_size(geometry.size.to_physical_precise_round(output_scale));
    let window_of = |id: u64| state.mapped_window(id);
    let locked = state.locked();
    let mut elements: Vec<OutputRenderElement<R>> = dim.into_iter().collect();
    let hosted = |renderer: &mut R, elements: &mut Vec<OutputRenderElement<R>>, below: bool| {
        for host in state.hosts.iter().rev().filter(|host| {
            if locked {
                // While locked only what stands over the lock shows, over the lock client: the
                // session's own lock view when the lock screen's app cannot show it, and the
                // readout that answers a hardware key pressed there.
                host.above_lock()
            } else {
                host.below_windows() == below
            }
        }) {
            for picture in host.window_pictures().iter().rev() {
                if let Some(window) = window_of(picture.window) {
                    elements.extend(crate::effects::pictured(
                        renderer,
                        state.framing(window),
                        window,
                        picture,
                        crate::capture::protected(state, window),
                        geometry.loc.to_f64(),
                        (bounds, output_scale),
                    ));
                }
            }
            for layer in host.layers().into_iter().rev() {
                let location: Point<i32, Physical> =
                    (Point::from(layer.location) - geometry.loc).to_physical_precise_round(scale);
                match hosted_layer(
                    renderer,
                    &state.uploads,
                    &layer,
                    location.to_f64(),
                    output_scale,
                ) {
                    Ok(Some(element)) => elements.push(element),
                    Ok(None) => {}
                    Err(error) => tracing::warn!(?error, "hosted layer: buffer not imported"),
                }
            }
        }
    };
    if locked {
        // The session's own readout answers a key pressed on the lock screen and shows over the
        // lock: the elements are front first, so it comes before the lock client's surfaces.
        hosted(renderer, &mut elements, false);
        elements.extend(state.lock_elements(renderer, output));
        return elements;
    }
    elements.extend(state.drag_icon_elements(renderer, output));
    elements.extend(state.input_popup_elements(renderer, output));
    // The layer surfaces above the windows: the bar and its panels.
    elements.extend(state.layer_elements(renderer, output, true, geometry, output_scale));
    hosted(renderer, &mut elements, false);
    elements.extend(window_elements(
        renderer,
        state,
        geometry.loc.to_f64(),
        output_scale,
        bounds,
        state
            .space
            .elements_for_output(output)
            .rev()
            .cloned()
            .collect(),
    ));
    hosted(renderer, &mut elements, true);
    // The layer surfaces below the windows: the desk.
    elements.extend(state.layer_elements(renderer, output, false, geometry, output_scale));
    elements
}

/// The elements `output` shows, front first, in its physical pixels: the scene, with the magnifier,
/// the pointer's crosshair and the visual alert over it.
pub(crate) fn output_elements<R>(
    renderer: &mut R,
    state: &State,
    output: &Output,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem + ImportDma,
    R::TextureId: Clone + Send + 'static,
{
    let Some(geometry) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let output_scale = output.current_scale().fractional_scale();
    let scale = Scale::from(output_scale);
    let bounds = Rectangle::from_size(geometry.size.to_physical_precise_round(output_scale));
    let mut elements = scene_elements(renderer, state, output);
    let visual = a11y::visual(output);
    if state.zoom > 1.0
        && let Some(centre) = a11y::follow(output, state, geometry, scale, Instant::now())
    {
        match visual.magnifier_view {
            // The whole scene is scaled about the point, which stays where it really is on screen.
            MagnifierView::FullScreen => {
                elements = elements
                    .into_iter()
                    .map(|element| zoomed(element, centre, state.zoom))
                    .collect();
            }
            // A lens over the scene: the scene drawn again, magnified and kept inside a rectangle
            // around the point, over the scene under it as it is.
            MagnifierView::Lens => {
                let lens = a11y::lens_rect(centre, bounds.size);
                let mut stack: Vec<OutputRenderElement<R>> =
                    scene_elements(renderer, state, output)
                        .into_iter()
                        .filter_map(|element| lensed(element, centre, state.zoom, lens, scale))
                        .collect();
                // The lens is opaque: where the scene has nothing to show under it, it shows the
                // background, never the unmagnified picture showing through.
                stack.push(OutputRenderElement::Dim(solid(
                    lens,
                    Color32F::new(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 1.0),
                )));
                stack.append(&mut elements);
                elements = stack;
            }
        }
    }
    // The pointer's crosshair, over the scene and its magnifier: guides at where the pointer really
    // is.
    if visual.crosshair && !state.locked() {
        let pointer: Point<i32, Physical> =
            (state.pointer - geometry.loc.to_f64()).to_physical_precise_round(scale);
        if bounds.contains(pointer) {
            elements.splice(
                0..0,
                crosshair_elements(bounds, pointer)
                    .into_iter()
                    .map(OutputRenderElement::Dim),
            );
        }
    }
    // The visual alert lights the inside of the display's edge, over everything else: it stays
    // obvious without covering what is on screen or washing the whole display with a luminance
    // change, which is a photosensitivity risk.
    let alert = visual.flash_alpha(Instant::now());
    if alert > 0.0 {
        let width = alert_edge_width(scale.x);
        let colour = Color32F::new(ALERT_EDGE[0], ALERT_EDGE[1], ALERT_EDGE[2], alert);
        elements.splice(
            0..0,
            alert_bands(bounds, width, colour)
                .into_iter()
                .map(OutputRenderElement::Dim),
        );
    }
    elements
}

/// The accent the visual alert's edge wears: the desktop's own accent, which reads against both a
/// dark and a light wallpaper.
const ALERT_EDGE: [f32; 3] = [0.246, 0.393, 0.752];

/// How wide the alert's edge is, in logical pixels: a band along each edge,
/// narrow enough to leave the screen's content alone and wide enough to see
/// from across a desk.
const ALERT_EDGE_WIDTH: f64 = 3.5;

/// The width of the alert's edge in the physical pixels of an output drawn at `scale` : never
/// nothing, so a fractional scale still shows it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a few pixels of a display, far inside i32"
)]
fn alert_edge_width(scale: f64) -> i32 {
    (ALERT_EDGE_WIDTH * scale).round().max(1.0) as i32
}

/// The four bands of the visual alert's edge, inside `bounds` (output physical pixels), `width`
/// pixels wide.
fn alert_bands(
    bounds: Rectangle<i32, Physical>,
    width: i32,
    colour: Color32F,
) -> [SolidColorRenderElement; 4] {
    let (w, h) = (bounds.size.w, bounds.size.h);
    // An output narrower than two edges plus something to show is all edge;
    // it still shows the alert, never a band outside itself.
    let width = width.clamp(1, (w.min(h) / 2).max(1));
    let (x, y) = (bounds.loc.x, bounds.loc.y);
    let band = |x0: i32, y0: i32, w: i32, h: i32| {
        solid(
            Rectangle::new(Point::from((x0, y0)), Size::from((w, h))),
            colour,
        )
    };
    [
        band(x, y, w, width),
        band(x, y + h - width, w, width),
        band(x, y, width, h),
        band(x + w - width, y, width, h),
    ]
}

#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Rectangle, Size};

    /// The area `bounds` keeps out of `covers`, as a rect for each piece.
    fn outside(
        bounds: Rectangle<i32, smithay::utils::Physical>,
        covers: &[Rectangle<i32, smithay::utils::Physical>],
    ) -> Vec<Rectangle<i32, smithay::utils::Physical>> {
        super::outside(bounds, covers)
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, smithay::utils::Physical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn area(pieces: &[Rectangle<i32, smithay::utils::Physical>]) -> i32 {
        pieces.iter().map(|piece| piece.size.w * piece.size.h).sum()
    }

    /// No piece stands on a cover, and none overlaps another.
    fn apart(
        pieces: &[Rectangle<i32, smithay::utils::Physical>],
        covers: &[Rectangle<i32, smithay::utils::Physical>],
    ) {
        for piece in pieces {
            for cover in covers {
                assert!(
                    piece.intersection(*cover).is_none(),
                    "{piece:?} stands on {cover:?}"
                );
            }
            for other in pieces {
                assert!(
                    piece == other || piece.intersection(*other).is_none(),
                    "{piece:?} overlaps {other:?}"
                );
            }
        }
    }

    #[test]
    fn a_cover_in_the_middle_leaves_the_rest_of_the_window() {
        let bounds = rect(0, 0, 100, 100);
        let covers = [rect(20, 30, 40, 20)];
        let pieces = outside(bounds, &covers);
        assert_eq!(
            area(&pieces),
            10_000 - 800,
            "everything but the cover: {pieces:?}"
        );
        apart(&pieces, &covers);
    }

    #[test]
    fn covers_overlapping_the_edges_take_only_what_they_cover() {
        let bounds = rect(10, 10, 60, 40);
        let covers = [rect(0, 0, 30, 30), rect(50, 40, 100, 100)];
        let pieces = outside(bounds, &covers);
        let covered = 20 * 20 + 20 * 10;
        assert_eq!(
            area(&pieces),
            60 * 40 - covered,
            "the parts of the covers inside the window: {pieces:?}"
        );
        apart(&pieces, &covers);
    }

    #[test]
    fn a_cover_over_everything_leaves_nothing() {
        assert!(outside(rect(0, 0, 50, 50), &[rect(-10, -10, 100, 100)]).is_empty());
    }

    #[test]
    fn a_cover_beside_the_window_takes_nothing() {
        let bounds = rect(0, 0, 50, 50);
        let pieces = outside(bounds, &[rect(50, 0, 10, 10)]);
        assert_eq!(pieces, vec![bounds]);
    }
}
