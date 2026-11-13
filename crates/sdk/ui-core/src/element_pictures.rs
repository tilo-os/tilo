//! Pictures a renderer copies on the GPU from where the page shows them. In a browser a video's
//! pictures are decoded by the page's media element.

use std::cell::RefCell;
use std::rc::Rc;

use vello::peniko::ImageData;

/// Pictures waiting for a renderer at most: one per playing video in a
/// frame. Past them the oldest go, so a page whose renderer never takes
/// them has no more.
const WAITING: usize = 8;

/// A picture to copy from `element` (the page's video element, a `wasm_bindgen::JsValue`) into the
/// pixels `image` draws from.
#[derive(Clone)]
pub struct ElementPicture {
    pub image: ImageData,
    pub element: Rc<dyn std::any::Any>,
}

thread_local! {
    static WAITING_PICTURES: RefCell<Vec<ElementPicture>> = const { RefCell::new(Vec::new()) };
}

/// `image` draws the picture `element` shows now: the renderer copies it before it renders the next
/// frame.
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
