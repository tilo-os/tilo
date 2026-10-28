//! Video pictures copied on the GPU from the page's media elements.
//!
//! A playing video shows each new picture as an image with no pixels of its
//! own (`ui::element_pictures`). Before a frame renders, [`VideoPictures`]
//! copies each element's current picture into a texture of that element's
//! and has the renderer draw the picture's image from it: the pixels never
//! pass through the page's thread.

use vello::peniko::ImageData;
use vello::wgpu;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;

/// Elements whose last picture stays drawable at most; past them the one
/// shown longest ago lets its picture go.
const ELEMENTS: usize = 4;

struct Shown {
    element: web_sys::HtmlVideoElement,
    texture: wgpu::Texture,
    /// The image the texture's picture draws, drawn from it until the
    /// element's next picture.
    image: ImageData,
}

/// Each element's texture and the image it holds the picture of.
#[derive(Default)]
pub(crate) struct VideoPictures {
    /// Oldest shown first.
    shown: Vec<Shown>,
}

impl VideoPictures {
    /// Copy the pictures shown since the last frame into their textures,
    /// for `renderer` to draw their images from.
    pub(crate) fn copy(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut vello::Renderer,
    ) {
        let max_side = device.limits().max_texture_dimension_2d;
        for picture in ui::element_pictures::take() {
            let Some(element) = picture
                .element
                .downcast_ref::<JsValue>()
                .and_then(|value| value.clone().dyn_into::<web_sys::HtmlVideoElement>().ok())
            else {
                continue;
            };
            let (width, height) = (picture.image.width, picture.image.height);
            // Copying a picture the element no longer has (a new source of
            // another size, one not yet decoded) is an error the page would
            // raise: such a picture is skipped, its image drawn transparent
            // until the next one.
            if width == 0
                || height == 0
                || width > max_side
                || height > max_side
                || element.ready_state() < web_sys::HtmlMediaElement::HAVE_CURRENT_DATA
                || element.video_width() != width
                || element.video_height() != height
            {
                continue;
            }
            let at = self.shown.iter().position(|s| s.element == element);
            let mut shown = if let Some(shown) = at.map(|at| self.shown.remove(at)) {
                renderer.override_image(&shown.image, None);
                shown
            } else {
                if self.shown.len() >= ELEMENTS {
                    let oldest = self.shown.remove(0);
                    renderer.override_image(&oldest.image, None);
                }
                Shown {
                    texture: picture_texture(device, width, height),
                    element: element.clone(),
                    image: picture.image.clone(),
                }
            };
            if shown.texture.width() != width || shown.texture.height() != height {
                shown.texture = picture_texture(device, width, height);
            }
            queue.copy_external_image_to_texture(
                &wgpu::CopyExternalImageSourceInfo {
                    source: wgpu::ExternalImageSource::HTMLVideoElement(element),
                    origin: wgpu::Origin2d::ZERO,
                    flip_y: false,
                },
                wgpu::CopyExternalImageDestInfo {
                    texture: &shown.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                    color_space: wgpu::PredefinedColorSpace::Srgb,
                    premultiplied_alpha: false,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            shown.image = picture.image;
            renderer.override_image(
                &shown.image,
                Some(wgpu::TexelCopyTextureInfoBase {
                    texture: shown.texture.clone(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                }),
            );
            self.shown.push(shown);
        }
    }
}

/// A texture the renderer copies a picture from into its image atlas.
fn picture_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("video picture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
