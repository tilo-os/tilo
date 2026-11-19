//! `#[derive(Row)]`: a plain struct as a declared row type — the type of the
//! entities a surface publishes that live outside any document (processes,
//! devices, containers, journal entries, fetched catalogue rows), which
//! search names to people exactly as it names a resource's items and which
//! offers its own effects.

use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, LitStr};

use crate::{humanize, krate};

#[derive(Default)]
struct RowOptions {
    kind: Option<String>,
    label: Option<String>,
    effects: Option<syn::Path>,
}

fn row_options(input: &DeriveInput) -> syn::Result<RowOptions> {
    let mut options = RowOptions::default();
    for attr in input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("row"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("kind") {
                options.kind = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("label") {
                options.label = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("effects") {
                options.effects = Some(meta.value()?.parse::<syn::Path>()?);
            } else {
                return Err(
                    meta.error("expected `kind = \"...\"`, `label = \"...\"` or `effects = Type`")
                );
            }
            Ok(())
        })?;
    }
    Ok(options)
}

pub(crate) fn derive(input: &DeriveInput) -> syn::Result<Tokens> {
    let krate = krate();
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "a row type has no generic parameters",
        ));
    }
    if !matches!(input.data, Data::Struct(_)) {
        return Err(syn::Error::new(
            input.span(),
            "`Row` is derived for a struct: one field per attribute the type carries",
        ));
    }
    let options = row_options(input)?;
    let kind = options.kind.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "name the row's kind, as its app's other kinds are: `#[row(kind = \"app.thing\")]`",
        )
    })?;
    let label = options.label.unwrap_or_else(|| humanize(&name.to_string()));
    let (effects, effects_with) = if let Some(effects) = &options.effects {
        (
            quote! {
                <#effects as #krate::Effect>::row_specs(
                    <Self as #krate::words::RowType>::KIND,
                    id,
                )
            },
            quote! {
                <#effects as #krate::Effect>::row_specs_with(
                    <Self as #krate::words::RowType>::KIND,
                    id,
                    bindings,
                )
            },
        )
    } else {
        (
            quote!(::std::vec::Vec::new()),
            quote!(::std::vec::Vec::new()),
        )
    };
    Ok(quote! {
        impl #krate::words::RowType for #name {
            const KIND: &'static str = #kind;
            const LABEL: &'static str = #label;

            fn effects(id: &#krate::Id) -> ::std::vec::Vec<#krate::__private::ActionSpec> {
                #effects
            }

            fn effects_with(
                id: &#krate::Id,
                bindings: &#krate::row::RowBindings,
            ) -> ::std::vec::Vec<#krate::__private::ActionSpec> {
                #effects_with
            }

        }
    })
}
