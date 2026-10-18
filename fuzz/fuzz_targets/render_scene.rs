//! The render service's view of an app: every byte of a frame record and of
//! a control message is hostile. Whatever the fuzzer writes must never panic
//! the decoder, the validator or, for what they accept, vello's resolver,
//! which is the CPU half of drawing it on the room's GPU.

#![no_main]

use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use peniko::{Blob, FontData, ImageAlphaType, ImageData, ImageFormat};
use tilo_render_protocol::control::{AppMessage, decode};
use tilo_render_protocol::frame::{self, LayerChange};
use tilo_render_protocol::scene::{self, Resources};
use tilo_render_protocol::validate::validate;
use tilo_render_protocol::{FontId, ImageId, Limits};
use vello_encoding::{Encoding, Resolver};

/// One font and one image registered, as a connection would have them.
struct Registered {
    font: FontData,
    image: ImageData,
}

impl Resources for Registered {
    fn font(&self, id: FontId) -> Option<FontData> {
        (id.0 == 0).then(|| self.font.clone())
    }

    fn image(&self, id: ImageId) -> Option<ImageData> {
        (id.0 == 0).then(|| self.image.clone())
    }
}

fn registered() -> &'static Registered {
    static REGISTERED: OnceLock<Registered> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let font =
            std::fs::read(tilo_font_catalog::checkout_root().join("AdwaitaSans-Regular.ttf"))
                .unwrap_or_default();
        Registered {
            font: FontData::new(Blob::new(Arc::new(font)), 0),
            image: ImageData {
                data: Blob::new(Arc::new(vec![0x7f_u8; 4 * 3 * 4])),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: 4,
                height: 3,
            },
        }
    })
}

fn resolve(encoding: &Encoding) {
    let mut packed = Vec::new();
    Resolver::new().resolve(encoding, &mut packed);
}

fuzz_target!(|data: &[u8]| {
    let resources = registered();
    let limits = Limits::default();
    if let Ok(checked) = frame::read(data, resources, &limits) {
        for layer in &checked.frame.layers {
            if let LayerChange::Content(content) = &layer.change {
                resolve(content);
            }
        }
        for backdrop in &checked.frame.backdrops {
            resolve(&backdrop.below);
        }
    }
    if let Ok(encoding) = scene::decode(data, resources)
        && validate(&encoding, &limits).is_ok()
    {
        resolve(&encoding);
    }
    let _ = decode::<AppMessage>(data);
});
