//! The `#[view]` attribute: a Rust app's view of a resource, declared where it is written.
//!
//! ```ignore
//! /// The channel's page: who it is, and its videos.
//! #[view(default)]
//! fn page(c: &Channel, cx: &mut ViewCx) -> Node { … }
//! ```
//!
//! The attribute leaves the function as it is — an ordinary Rust function a test can call — and
//! adds a registration beside it, so the function is found by the resource type's kind and the
//! view's name without anything listing it (one-app-model.md §2: a view is a named function of a
//! resource, and the shell places it by resource and view name).
//!
//! The name is the function's own. `#[view(default)]` marks the one a place shows a resource with
//! when it names none. The resource type is the function's first parameter, which names its kind
//! and app through [`ViewResource`](::tilo_view::ViewResource) — the resource's `#[resource]`
//! derives that, so a type that is a resource needs nothing more.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{ItemFn, Token, parse_macro_input};

/// `#[view]` or `#[view(default)]`.
#[proc_macro_attribute]
pub fn view(attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    match expand(&attr.into(), &function) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand(attr: &Tokens, function: &ItemFn) -> syn::Result<Tokens> {
    let is_default = options(attr)?;
    let name = function.sig.ident.clone();
    if !function.sig.generics.params.is_empty() {
        return Err(syn::Error::new(
            function.sig.generics.span(),
            "a view is a function of one resource: it takes no generics",
        ));
    }
    let inputs = &function.sig.inputs;
    let first = inputs.first().ok_or_else(|| {
        syn::Error::new(
            function.sig.span(),
            "a view takes the resource it shows first: `fn page(c: &Channel, cx: &mut ViewCx)`",
        )
    })?;
    let syn::FnArg::Typed(first) = first else {
        return Err(syn::Error::new(
            first.span(),
            "a view takes the resource it shows first, not `self`",
        ));
    };
    let resource = match &*first.ty {
        syn::Type::Reference(reference) => (*reference.elem).clone(),
        other => other.clone(),
    };
    let name_text = name.to_string();
    let adapter = format_ident!("__view_{}", name);

    // A page registers no view: nothing on it draws a Rust app's view, and a
    // registration is a constructor the page runs at start, which would carry
    // every app's views into the module it downloads first instead of the
    // app's own.
    Ok(quote! {
        #[cfg_attr(
            target_arch = "wasm32",
            allow(dead_code, reason = "a page registers no view")
        )]
        #function

        #[cfg(not(target_arch = "wasm32"))]
        #[allow(
            non_snake_case,
            reason = "the registration is named for the view it stands beside"
        )]
        fn #adapter(
            input: &::tilo_view::registry::ViewInput<'_>,
            cx: &::tilo_view::ViewCx,
        ) -> ::tilo_view::Node {
            // A resource whose reading has not arrived yet is its type's default: the view
            // shows what it can without it, and runs again when the reading comes.
            let reading = input
                .reading
                .cloned()
                .unwrap_or(::tilo_view::serde_json::Value::Null);
            let resource: #resource = ::tilo_view::serde_json::from_value(reading)
                .unwrap_or_default();
            #name(&resource, cx)
        }

        #[cfg(not(target_arch = "wasm32"))]
        ::tilo_view::inventory::submit! {
            ::tilo_view::View {
                name: #name_text,
                kind: <#resource as ::tilo_view::ViewResource>::KIND,
                is_default: #is_default,
                render: #adapter,
                app: <#resource as ::tilo_view::ViewResource>::APP,
            }
        }
    })
}

/// Whether the attribute says `default`.
fn options(attr: &Tokens) -> syn::Result<bool> {
    if attr.is_empty() {
        return Ok(false);
    }
    let parser = |input: syn::parse::ParseStream<'_>| -> syn::Result<bool> {
        let key: syn::Ident = input.parse()?;
        if key != "default" {
            return Err(syn::Error::new(key.span(), "expected `default`"));
        }
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
            if !input.is_empty() {
                return Err(input.error("`default` is the only argument a view takes"));
            }
        }
        Ok(true)
    };
    syn::parse::Parser::parse2(parser, attr.clone())
}
