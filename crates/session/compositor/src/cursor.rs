//! The pointer's image: the surface a client set, or a named shape from the cursor theme.
//! Backends that own the screen draw it in front of everything else (the DRM backend's
//! `DrmCompositor` puts it on the cursor plane where there is one). Named shapes come from the
//! cursor theme the person chose ([`CursorA11y::theme`](crate::CursorA11y::theme)), or, until they
//! choose one, the theme `XCURSOR_THEME` names (`default` otherwise), at `XCURSOR_SIZE` (24) times
//! the output's scale; without a theme a plain arrow is drawn.
//! A named image's buffer is made at the output's scale, so every renderer draws its pixels at the
//! size the person set: the composited scene and a hardware cursor plane alike. Carrying the scale
//! in the buffer rather than in an element size override is what makes the plane agree; see
//! [`Image`].

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical cursor positions, far from i32 limits"
)]

use std::cell::RefCell;
use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::render::OutputRenderElement;
use crate::state::State;

/// The cursor size, in logical pixels, when `XCURSOR_SIZE` does not say.
const DEFAULT_SIZE: u32 = 24;

/// The cursor theme drawn when neither the person nor `XCURSOR_THEME` names one.
const DEFAULT_THEME: &str = "default";

/// One image of a named shape at one size.
///
/// Its pixels are the output's: an image loaded at the person's size times
/// the output's scale, so drawing them one to one is the size they set.
///
/// Its buffer says so, because the hardware cursor plane only understands
/// a buffer: Smithay's `DrmCompositor` copies a `Kind::Cursor` element into
/// the plane's buffer with `copy_element_to_cursor_bo`, which takes the
/// element's `src()` — the buffer's own scale at work — and blits exactly
/// those pixels into the plane. It refuses what it would have to crop or
/// scale. So a 48-pixel image in a scale-2 buffer is 24 logical pixels and
/// copies whole, while the same image in a scale-1 buffer with a
/// 24-logical-pixel element size has an `src` of 24 of its pixels covered
/// by 48: the plane declines it, and a path that did honour it would show
/// the top-left quarter of the pointer at twice its size.
#[derive(Clone)]
struct Image {
    buffer: MemoryRenderBuffer,
    /// Its size in pixels.
    size: Size<i32, Physical>,
    /// The hotspot, in the image's pixels.
    hotspot: Point<i32, Physical>,
    /// The logical pixels its buffer is one whole number of: the output scale it was loaded for,
    /// rounded.
    buffer_scale: i32,
}

impl Image {
    /// The part of its buffer its element draws: all of it, in the logical
    /// units its buffer's scale names.
    ///
    /// Stated rather than left to `from_buffer`, whose default is the
    /// element's logical size: right when the buffer's scale is the
    /// output's, but a fractional display scale rounds the buffer's scale,
    /// and the default would then sample less of the image than it has or
    /// read past it.
    fn src(&self) -> Rectangle<f64, Logical> {
        Rectangle::from_size(self.size.to_f64().to_logical(f64::from(self.buffer_scale)))
    }

    /// The size its element is drawn at, in the logical pixels of an output at `scale` : the size
    /// the person set.
    fn logical_size(&self, scale: f64) -> Size<i32, Logical> {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.size.to_f64().to_logical(scale).to_i32_round()
    }
}

/// What the pointer shows, the theme it is drawn from and the theme images loaded so far.
pub(crate) struct Cursor {
    pub(crate) status: CursorImageStatus,
    theme: xcursor::CursorTheme,
    /// The name `theme` was loaded by, which Xwayland's clients are told.
    theme_name: String,
    /// The theme the person chose; empty leaves the environment's in force.
    chosen_theme: String,
    size: u32,
    images: RefCell<HashMap<(CursorIcon, u32), Image>>,
}

impl Cursor {
    pub(crate) fn new() -> Self {
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|size| size.parse().ok())
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_SIZE);
        let theme_name = theme_in_force("");
        Self {
            status: CursorImageStatus::default_named(),
            theme: xcursor::CursorTheme::load(&theme_name),
            theme_name,
            chosen_theme: String::new(),
            size,
            images: RefCell::new(HashMap::new()),
        }
    }

    /// Draw the pointer from the theme `theme` names, loading it now when it is not the theme in
    /// use; an empty `theme` leaves the environment's (`XCURSOR_THEME`, else `default` ) in force.
    pub(crate) fn set_theme(&mut self, theme: &str) {
        if theme == self.chosen_theme {
            return;
        }
        theme.clone_into(&mut self.chosen_theme);
        self.theme_name = theme_in_force(theme);
        self.theme = xcursor::CursorTheme::load(&self.theme_name);
        // Images are cached per theme as much as per size: the old ones only take room.
        self.images.borrow_mut().clear();
    }

    /// The name of the theme the pointer is drawn from.
    pub(crate) fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// Show the pointer at `size` logical pixels, inside what a person can still aim with; `1` when
    /// it is not set.
    pub(crate) fn set_size(&mut self, size: u32) {
        let size = size.clamp(1, crate::a11y::MAX_CURSOR_SIZE);
        if size == self.size {
            return;
        }
        self.size = size;
        // Images are cached per pixel size, so the old ones only take room.
        self.images.borrow_mut().clear();
    }

    /// The image of `icon` for the person's `size` logical pixels on an output at `scale` , loaded
    /// on first use: the theme's image nearest those pixels, its buffer at that scale.
    fn image(&self, icon: CursorIcon, size: u32, scale: f64) -> Image {
        let pixels = cursor_pixels(size, scale);
        let theme = &self.theme;
        self.images
            .borrow_mut()
            .entry((icon, pixels))
            .or_insert_with(|| {
                themed(theme, icon, pixels, scale).unwrap_or_else(|| arrow(pixels, scale))
            })
            .clone()
    }
}

/// The theme directory to load for the person's `chosen` theme: that one once they chose it, else
/// the environment's (`XCURSOR_THEME`), else the plain `default` theme.
fn theme_in_force(chosen: &str) -> String {
    if !chosen.is_empty() {
        return chosen.to_owned();
    }
    std::env::var("XCURSOR_THEME").unwrap_or_else(|_| DEFAULT_THEME.to_owned())
}

/// `icon` from the theme, under its name or an older alias, at the size nearest `pixels` , its
/// buffer at `scale` .
fn themed(
    theme: &xcursor::CursorTheme,
    icon: CursorIcon,
    pixels: u32,
    scale: f64,
) -> Option<Image> {
    let path = std::iter::once(icon.name())
        .chain(icon.alt_names().iter().copied())
        .find_map(|name| theme.load_icon(name))?;
    let bytes = std::fs::read(path).ok()?;
    let images = xcursor::parser::parse_xcursor(&bytes)?;
    let image = images
        .iter()
        .min_by_key(|image| image.size.abs_diff(pixels))?;
    let size = (
        i32::try_from(image.width).ok()?,
        i32::try_from(image.height).ok()?,
    );
    Some(Image {
        // RGBA bytes are ABGR8888 in DRM's little-endian naming.
        buffer: MemoryRenderBuffer::from_slice(
            &image.pixels_rgba,
            Fourcc::Abgr8888,
            size,
            buffer_scale(scale),
            Transform::Normal,
            None,
        ),
        size: Size::from(size),
        hotspot: Point::from((
            i32::try_from(image.xhot).ok()?,
            i32::try_from(image.yhot).ok()?,
        )),
        buffer_scale: buffer_scale(scale),
    })
}

/// A plain arrow, white with a black edge, `pixels` tall, for systems with no cursor theme, its
/// buffer at `scale` .
fn arrow(pixels: u32, scale: f64) -> Image {
    let side = pixels.max(8);
    let rgba = arrow_pixels(side);
    let extent = i32::try_from(side).unwrap_or(24);
    Image {
        buffer: MemoryRenderBuffer::from_slice(
            &rgba,
            Fourcc::Abgr8888,
            (extent, extent),
            buffer_scale(scale),
            Transform::Normal,
            None,
        ),
        size: Size::from((extent, extent)),
        hotspot: Point::from((0, 0)),
        buffer_scale: buffer_scale(scale),
    }
}

/// RGBA rows of a `side` -pixel arrow: a right triangle with its point at the top left, as wide as
/// it is tall at the bottom row's half.
fn arrow_pixels(side: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        // Each row is half a pixel wider than the last.
        let width = u32::midpoint(y, 2);
        for x in 0..side {
            let pixel: [u8; 4] = if x > width {
                [0, 0, 0, 0]
            } else if x == 0 || x == width || y + 1 == side {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

impl State {
    /// What the pointer shows now: a hosted surface under it chooses its shape, else the focused
    /// client.
    fn cursor_status(&self) -> CursorImageStatus {
        match self.host_under_pointer() {
            Some(host) => CursorImageStatus::Named(host.cursor()),
            None => self.cursor.status.clone(),
        }
    }

    /// The pointer's image at `scale` : its size and hotspot in physical pixels; `None` while it is
    /// hidden.
    pub(crate) fn cursor_shape(
        &self,
        scale: f64,
    ) -> Option<(Size<i32, Physical>, Point<i32, Physical>)> {
        match self.cursor_status() {
            CursorImageStatus::Hidden => None,
            CursorImageStatus::Surface(surface) => {
                let hotspot = surface_hotspot(&surface);
                let size = bbox_from_surface_tree(&surface, (0, 0)).size;
                Some((
                    size.to_physical_precise_round(scale),
                    hotspot.to_physical_precise_round(scale),
                ))
            }
            CursorImageStatus::Named(icon) => {
                let image = self.cursor.image(icon, self.cursor.size, scale);
                Some((image.size, image.hotspot))
            }
        }
    }

    /// The pointer's elements on `output` , front first, in its physical pixels; none when the
    /// pointer is elsewhere or hidden.
    pub(crate) fn cursor_elements<R>(
        &self,
        renderer: &mut R,
        output: &Output,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let Some(geometry) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        if !geometry.to_f64().contains(self.pointer) {
            return Vec::new();
        }
        let scale = output.current_scale().fractional_scale();
        let at: Point<i32, Physical> =
            (self.pointer - geometry.loc.to_f64()).to_physical_precise_round(scale);
        self.cursor_elements_at(renderer, at, scale)
    }

    /// The pointer's elements with its hotspot at `at` (physical pixels of whatever they are drawn
    /// into) at `scale` ; none while it is hidden.
    pub(crate) fn cursor_elements_at<R>(
        &self,
        renderer: &mut R,
        at: Point<i32, Physical>,
        scale: f64,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        match self.cursor_status() {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = surface_hotspot(&surface);
                let origin = at - hotspot.to_physical_precise_round(scale);
                render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    origin,
                    Scale::from(scale),
                    1.0,
                    Kind::Cursor,
                )
            }
            CursorImageStatus::Named(icon) => {
                let image = self.cursor.image(icon, self.cursor.size, scale);
                let origin = at - image.hotspot;
                match named_element(renderer, &image, origin, scale) {
                    Ok(element) => vec![element],
                    Err(error) => {
                        tracing::warn!(?error, "cursor image not imported");
                        Vec::new()
                    }
                }
            }
        }
    }
}

/// The element a named pointer `image` draws as, with its top left at `origin` (the output's
/// physical pixels) on an output at `scale` .
/// The image's pixels are the output's, so the element draws all of them over the logical extent
/// its buffer's scale names: at 2 a 48-pixel pointer image covers 24 logical pixels at those 48
/// device pixels.
fn named_element<R>(
    renderer: &mut R,
    image: &Image,
    origin: Point<i32, Physical>,
    scale: f64,
) -> Result<OutputRenderElement<R>, R::Error>
where
    R: ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    Ok(OutputRenderElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            origin.to_f64(),
            &image.buffer,
            None,
            Some(image.src()),
            Some(image.logical_size(scale)),
            Kind::Cursor,
        )?,
    ))
}

/// The scale a named image's buffer is made at, from the output's: the nearest whole device pixels
/// per logical pixel, at least `1` .
/// The buffer scale is what makes the drawn size the pointer's: an element takes its `src` from its
/// buffer, so a 48-pixel image in a scale-2 buffer is 24 logical pixels and covers every one of its
/// 48 device pixels, which is what lets a hardware cursor plane copy it whole.
fn buffer_scale(scale: f64) -> i32 {
    if scale.is_finite() && scale >= 1.0 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a display's scale is far below i32::MAX"
        )]
        let whole = scale.round() as i32;
        whole.clamp(1, i32::from(u16::MAX))
    } else {
        1
    }
}

/// The hotspot a client set for its cursor surface, logical.
fn surface_hotspot(surface: &WlSurface) -> Point<i32, Logical> {
    smithay::wayland::compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .and_then(|data| data.lock().ok().map(|data| data.hotspot))
            .unwrap_or_default()
    })
}

/// The physical size of a `size` -logical-pixel cursor at `scale` .
fn cursor_pixels(size: u32, scale: f64) -> u32 {
    let pixels = (f64::from(size) * scale).round();
    if pixels.is_finite() && pixels >= 1.0 && pixels <= f64::from(u16::MAX) {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked to lie in 1..=u16::MAX above"
        )]
        let pixels = pixels as u32;
        pixels
    } else {
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::Color32F;
    use smithay::backend::renderer::element::Element as _;
    use smithay::backend::renderer::element::RenderElement as _;
    use smithay::backend::renderer::pixman::PixmanRenderer;
    use smithay::utils::Buffer;

    #[test]
    fn cursors_grow_with_the_output_scale() {
        assert_eq!(cursor_pixels(24, 1.0), 24);
        assert_eq!(cursor_pixels(24, 1.5), 36);
        assert_eq!(cursor_pixels(24, 2.0), 48);
        assert_eq!(cursor_pixels(24, f64::NAN), 24);
        assert_eq!(cursor_pixels(24, 0.0), 24);
    }

    /// The box the pointer's element draws into, in the output's physical pixels, from the elements
    /// [`State::cursor_elements_at`] makes for a pointer at the top left of an output at `scale` .
    fn drawn_box(scale: f64) -> (i32, i32) {
        let mut compositor = crate::Compositor::headless(Box::new(crate::StackPolicy))
            .expect("a headless compositor");
        let state = &mut compositor.data.state;
        state.cursor.set_size(24);
        let mut renderer = PixmanRenderer::new().expect("pixman");
        let elements: Vec<OutputRenderElement<PixmanRenderer>> =
            state.cursor_elements_at(&mut renderer, Point::<i32, Physical>::from((0, 0)), scale);
        let element = elements.first().expect("the pointer drew an element");
        let geometry = element.geometry(Scale::from(scale));
        (geometry.size.w, geometry.size.h)
    }

    /// The pointer covers the person's 24 logical pixels at every scale: at 2 it is drawn with
    /// twice the pixels of a 24-pixel pointer, not twice the size. An image loaded at 24 times the
    /// scale and then given a logical size of its own pixels would cover 48 logical pixels at 2.
    #[test]
    fn the_pointer_is_the_persons_size_in_logical_pixels_at_two() {
        let (plain_w, plain_h) = drawn_box(1.0);
        assert!(
            (plain_w - 24).abs() <= 1 && (plain_h - 24).abs() <= 1,
            "at 1 the pointer draws its 24 logical pixels: it drew {plain_w}x{plain_h}"
        );
        let (wide, tall) = drawn_box(2.0);
        assert!(
            (wide - 48).abs() <= 2 && (tall - 48).abs() <= 2,
            "at 2 the pointer covers the same 24 logical pixels, drawn at 48: \
             it drew {wide}x{tall}"
        );
    }

    /// A theme-shaped image as the production loader makes one for the
    /// person's 24 logical pixels on an output at `scale`: the plain arrow,
    /// with a bright dot in its bottom-right corner so a crop or an
    /// enlargement shows in the plane's pixels.
    fn image_at(scale: f64) -> Image {
        let pixels = cursor_pixels(24, scale);
        let side = usize::try_from(pixels).unwrap();
        let mut rgba = arrow_pixels(pixels);
        let at = (side - 2) * side + (side - 2);
        rgba[at * 4..at * 4 + 4].copy_from_slice(&[0xff, 0x00, 0xff, 0xff]);
        let extent = i32::try_from(pixels).unwrap();
        Image {
            buffer: MemoryRenderBuffer::from_slice(
                &rgba,
                Fourcc::Abgr8888,
                (extent, extent),
                buffer_scale(scale),
                Transform::Normal,
                None,
            ),
            size: Size::from((extent, extent)),
            hotspot: Point::from((0, 0)),
            buffer_scale: buffer_scale(scale),
        }
    }

    /// The pointer's element from `image` on an output at `scale` .
    fn element_of(
        renderer: &mut PixmanRenderer,
        image: &Image,
        scale: f64,
    ) -> OutputRenderElement<PixmanRenderer> {
        named_element(renderer, image, Point::<i32, Physical>::from((0, 0)), scale)
            .expect("the pointer imports")
    }

    /// What smithay's `DrmCompositor` puts in the hardware cursor plane's
    /// buffer for `element` on an output at `scale`, at the plane's `side`
    /// by `side` pixels: the render `try_assign_cursor_plane` makes.
    ///
    /// It first tries `copy_element_to_cursor_bo`, which blits the
    /// element's `src()` pixels one to one and refuses a crop or a scale;
    /// failing that it rasterises `src()` over a box the element's own
    /// geometry states — `frame.render_texture_from_to(texture, src,
    /// Rectangle::from_size(element_geometry.size), ..)` — so a `src`
    /// smaller than the box is stretched over it.
    fn cursor_plane_pixels(
        renderer: &mut PixmanRenderer,
        element: &OutputRenderElement<PixmanRenderer>,
        scale: f64,
        side: u32,
    ) -> Vec<[u8; 4]> {
        use smithay::backend::renderer::{Bind as _, ExportMem as _, Frame as _, Offscreen as _};
        let src = element.src();
        let geometry = element.geometry(Scale::from(scale));
        let extent = i32::try_from(side).unwrap();
        let plane = Size::<i32, Buffer>::from((extent, extent));
        let mut target = renderer
            .create_buffer(Fourcc::Abgr8888, plane)
            .expect("a plane buffer");
        let mut framebuffer = renderer.bind(&mut target).expect("bind");
        {
            let mut frame = smithay::backend::renderer::Renderer::render(
                renderer,
                &mut framebuffer,
                Size::<i32, Physical>::from((extent, extent)),
                Transform::Normal,
            )
            .expect("render");
            frame
                .clear(
                    Color32F::TRANSPARENT,
                    &[Rectangle::from_size(Size::<i32, Physical>::from((
                        extent, extent,
                    )))],
                )
                .expect("clear");
            let OutputRenderElement::Memory(memory) = element else {
                panic!("the pointer is a memory element");
            };
            memory
                .draw(
                    &mut frame,
                    src,
                    Rectangle::from_size(geometry.size),
                    &[Rectangle::from_size(geometry.size)],
                    &[],
                )
                .expect("draw the cursor into the plane");
            let _ = frame.finish().expect("finish");
        }
        let mapping = renderer
            .copy_framebuffer(&framebuffer, Rectangle::from_size(plane), Fourcc::Abgr8888)
            .expect("read the plane back");
        let bytes = renderer.map_texture(&mapping).expect("map").to_vec();
        bytes
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect()
    }

    /// The hardware cursor plane's buffer holds the whole pointer at the person's size on a 2
    /// display: the arrow's bright bottom-right pixel is there, and the image fills exactly 48 by
    /// 48 pixels of the plane.
    /// This is what a 2 display showed wrong: the element's `src` was 24 of the image's pixels over
    /// a 48-pixel box, so the plane held a doubled top-left quarter — the pointer twice the size,
    /// its lower half cut off.
    #[test]
    fn the_cursor_plane_holds_the_whole_pointer_at_forty_eight_pixels() {
        let mut renderer = PixmanRenderer::new().expect("pixman");
        let image = image_at(2.0);
        let element = element_of(&mut renderer, &image, 2.0);
        let plane = cursor_plane_pixels(&mut renderer, &element, 2.0, 64);
        let pixel = |x: u32, y: u32| plane[usize::try_from(y * 64 + x).unwrap()];
        assert_eq!(
            pixel(46, 46),
            [0xff, 0x00, 0xff, 0xff],
            "the pointer's own last corner pixel is in the plane: a crop ends short of it"
        );
        assert_eq!(
            pixel(1, 1),
            [0x00, 0x00, 0x00, 0xff],
            "and its first, at one to one"
        );
        let occupied_last = (0..64).rfind(|&y| (0..64).any(|x| pixel(x, y)[3] > 0));
        assert_eq!(
            occupied_last,
            Some(47),
            "the image fills the plane's 48 rows, the person's 24 logical pixels \
             at 2: a doubled pointer ran to the plane's edge at 63"
        );
    }

    /// At 1 the plane holds the pointer whole too, at its 24 pixels.
    #[test]
    fn the_cursor_plane_holds_the_whole_pointer_at_twenty_four_pixels() {
        let mut renderer = PixmanRenderer::new().expect("pixman");
        let image = image_at(1.0);
        let element = element_of(&mut renderer, &image, 1.0);
        let plane = cursor_plane_pixels(&mut renderer, &element, 1.0, 64);
        let pixel = |x: u32, y: u32| plane[usize::try_from(y * 64 + x).unwrap()];
        assert_eq!(
            pixel(22, 22),
            [0xff, 0x00, 0xff, 0xff],
            "the last corner pixel"
        );
        let occupied_last = (0..64).rfind(|&y| (0..64).any(|x| pixel(x, y)[3] > 0));
        assert_eq!(
            occupied_last,
            Some(23),
            "the pointer's 24 pixels, uncropped"
        );
    }

    /// A fractional scale holds the pointer whole as well: the buffer says
    /// the nearest whole scale, the element states the size the person set,
    /// and the plane is handed all of the image's pixels.
    #[test]
    fn the_cursor_plane_holds_the_whole_pointer_at_a_fractional_scale() {
        let mut renderer = PixmanRenderer::new().expect("pixman");
        for (scale, pixels) in [(1.25_f64, 30_u32), (1.5, 36), (2.5, 60)] {
            let image = image_at(scale);
            let element = element_of(&mut renderer, &image, scale);
            let plane = cursor_plane_pixels(&mut renderer, &element, scale, 64);
            let pixel = |x: u32, y: u32| plane[usize::try_from(y * 64 + x).unwrap()];
            let last = pixels - 2;
            assert_eq!(
                pixel(last, last),
                [0xff, 0x00, 0xff, 0xff],
                "at {scale} the pointer's last pixel is in the plane, not a crop of it"
            );
            let occupied_last = (0..64).rfind(|&y| (0..64).any(|x| pixel(x, y)[3] > 0));
            assert_eq!(
                occupied_last,
                Some(pixels - 1),
                "at {scale} the plane holds exactly the image's pixels"
            );
        }
    }

    /// The drawn pointer covers the person's 24 logical pixels at every scale, whatever scale its
    /// image was loaded at.
    #[test]
    fn the_pointer_is_the_persons_size_in_logical_pixels_at_every_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0, 2.5, 3.0] {
            let pixels = cursor_pixels(24, scale);
            let image = Image {
                buffer: MemoryRenderBuffer::from_slice(
                    &arrow_pixels(pixels),
                    Fourcc::Abgr8888,
                    (
                        i32::try_from(pixels).unwrap(),
                        i32::try_from(pixels).unwrap(),
                    ),
                    buffer_scale(scale),
                    Transform::Normal,
                    None,
                ),
                size: Size::from((
                    i32::try_from(pixels).unwrap(),
                    i32::try_from(pixels).unwrap(),
                )),
                hotspot: Point::from((0, 0)),
                buffer_scale: buffer_scale(scale),
            };
            let logical = image.logical_size(scale);
            let shown = logical.to_f64().to_physical(scale);
            assert!(
                (f64::from(logical.w) - 24.0).abs() <= 1.0
                    && (f64::from(logical.h) - 24.0).abs() <= 1.0,
                "at {scale} the pointer covers the person's 24 logical pixels: it \
                 covers {logical:?}, which shows at {shown:?}"
            );
        }
    }

    /// The person's theme is the one the pointer is drawn from, and the one Xwayland's clients are
    /// told; until they choose one, the environment's is (`default` when it names none). Neither
    /// has to be installed here: a theme the machine lacks draws the plain arrow.
    #[test]
    fn the_pointer_draws_the_theme_the_person_chose() {
        let mut cursor = Cursor::new();
        let environment = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_owned());
        assert_eq!(cursor.theme_name(), environment);
        cursor.set_theme("Bibata-Modern-Ice");
        assert_eq!(cursor.theme_name(), "Bibata-Modern-Ice");
        cursor.set_theme("Adwaita");
        assert_eq!(cursor.theme_name(), "Adwaita");
        // A theme the person drew back to the environment's.
        cursor.set_theme("");
        assert_eq!(cursor.theme_name(), environment);
    }

    #[test]
    fn the_fallback_arrow_points_at_the_top_left() {
        let side = 16;
        let rgba = arrow_pixels(side);
        assert_eq!(rgba.len(), 16 * 16 * 4);
        let pixel = |x: usize, y: usize| {
            let at = (y * 16 + x) * 4;
            rgba.get(at..at + 4).map(<[u8]>::to_vec)
        };
        // The point is opaque, the far top right is clear, the middle of the bottom-left half is
        // white.
        assert_eq!(pixel(0, 0), Some(vec![0, 0, 0, 255]));
        assert_eq!(pixel(15, 0), Some(vec![0, 0, 0, 0]));
        assert_eq!(pixel(2, 10), Some(vec![255, 255, 255, 255]));
    }
}
