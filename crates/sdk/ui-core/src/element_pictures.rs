//! Pictures a renderer copies on the GPU from where the page shows them.
//!
//! In a browser a video's pictures are decoded by the page's media element.
//! Reading each one back to draw it as RGBA costs the page's one thread a
//! colour conversion and a copy of every pixel per picture: more than a
//! frame's time for a 1080p video on a software GPU. Instead the widget
//! draws an image with no pixels of its own, a new one per picture, and
//! [`show`]s which element holds its pixels; before rendering a frame, the
//! page's renderer [`take`]s them and copies each element's picture into a
//! texture the image draws from. A new picture so changes only the pixels
//! under the video: nothing is measured or laid out again.
//!
//! Off the web nothing is shown here: the media engine hands out no element
//! pictures there.

use std::cell::RefCell;
use std::rc::Rc;

use vello::peniko::ImageData;

/// Pictures waiting for a renderer at most: one per playing video in a
/// frame. Past them the oldest go, so a page whose renderer never takes
/// them holds no more.
const WAITING: usize = 8;

/// A picture to copy from `element` (the page's video element, a
/// `wasm_bindgen::JsValue`) into the pixels `image` draws from.
#[derive(Clone)]
pub struct ElementPicture {
    pub image: ImageData,
    pub element: Rc<dyn std::any::Any>,
}

thread_local! {
    static WAITING_PICTURES: RefCell<Vec<ElementPicture>> = const { RefCell::new(Vec::new()) };
}

/// `image` draws the picture `element` shows now: the renderer copies it
/// before it renders the next frame.
pub fn show(image: ImageData, element: Rc<dyn std::any::Any>) {
    WAITING_PICTURES.with(|waiting| {
        let mut waiting = waiting.borrow_mut();
        if waiting.len() >= WAITING {
            waiting.remove(0);
        }
        waiting.push(ElementPicture { image, element });
    });
}

/// The pictures shown since the last call, oldest first.
pub fn take() -> Vec<ElementPicture> {
    WAITING_PICTURES.with(|waiting| std::mem::take(&mut *waiting.borrow_mut()))
}
