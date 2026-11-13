//! [`Image`] as a described component: a picture by its address.

use crate::component::{Component, Described};
use crate::pictures::Pictures;

use super::{Image, ImageFit};

crate::component::choice!(ImageFit, "ImageFit", {
    Contain => "contain",
    Cover => "cover",
    Fill => "fill",
});

/// A picture: artwork, a photo, a cover, read from its address.
#[derive(Component, Default)]
#[component(name = "Image", role = "Image", label = "alt")]
pub struct ImageProps {
    /// Where the picture's bytes are: a content address (`tilo-blob://<hash>`) or a URL.
    #[component(required, positional)]
    #[component(data)]
    pub src: String,
    /// What the picture shows, as assistive technology announces it; without one it is
    /// decorative.
    #[component(required)]
    pub alt: Option<String>,
    /// How it fills its box: `contain`, `cover` or `fill`.
    pub fit: Option<ImageFit>,
    /// The window's pictures, which load its bytes; a host hands them in.
    #[component(skip)]
    pub pictures: Option<Pictures>,
}

impl Described for Image {
    type Props = ImageProps;

    fn from_props(_key: &str, props: ImageProps) -> Self {
        let ImageProps {
            src,
            alt,
            fit,
            pictures,
        } = props;
        let pictures = pictures.unwrap_or_else(crate::pictures::Pictures::window);
        let mut image = Self::from_picture(pictures, super::PictureSource { src });
        if let Some(alt) = alt {
            image = image.with_alt(alt);
        }
        if let Some(fit) = fit {
            image = image.with_fit(fit);
        }
        image
    }

    fn set_props(&mut self, props: ImageProps) -> bool {
        let ImageProps {
            src,
            alt,
            fit,
            pictures,
        } = props;
        let pictures = pictures.unwrap_or_else(crate::pictures::Pictures::window);
        self.set_picture(pictures, super::PictureSource { src });
        self.set_alt(alt);
        if let Some(fit) = fit {
            self.set_fit(fit);
        }
        true
    }
}
