//! The host calls on the fonts a device has, answered through its [`crate::fonts::FontHost`].

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

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
