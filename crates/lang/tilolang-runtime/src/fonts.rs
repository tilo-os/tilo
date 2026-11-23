//! The families text is set in on this device: what a reading face's picker offers.
//!
//! The host that runs the code answers it ([`FontHost`]): a window's host has the toolkit, whose
//! catalog holds the faces Tilo ships and the ones the machine installed. A host without the
//! toolkit answers none, and a view that offers a face's picker offers only the face it sets.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// Where a host's text-setting families come from.
pub trait FontHost {
    /// Every family this device can set text in, each once.
    fn families(&self) -> Vec<String>;
}

impl Core {
    /// `fonts()`: the families this device sets text in.
    pub(crate) fn font_import(&self, imp: Imp, _a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::FontFamilies => {
                let families = self
                    .fonts
                    .borrow()
                    .as_ref()
                    .map(|fonts| fonts.families())
                    .unwrap_or_default();
                self.ret_h(Value::List(families.into_iter().map(Value::text).collect()))
            }
            _ => trap("not a font import"),
        }
    }
}
