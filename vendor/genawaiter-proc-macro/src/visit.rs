use quote::quote;
use syn::{
    visit_mut::{self, VisitMut},
    Macro,
};

pub struct YieldReplace;

impl VisitMut for YieldReplace {
    // `yield_!(value)` is an expression macro or, followed by a semicolon, a
    // statement macro; both carry the same `Macro`, which gets the coroutine
    // argument threaded in front of its tokens.
    fn visit_macro_mut(&mut self, mac: &mut Macro) {
        if mac.path.segments.iter().any(|seg| seg.ident == "yield_") {
            let tkns = &mac.tokens;
            mac.tokens = quote! { @__impl => __private_co_arg__, #tkns };
        }

        visit_mut::visit_macro_mut(self, mac);
    }
}
