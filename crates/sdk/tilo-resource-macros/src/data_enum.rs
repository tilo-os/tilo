//! `#[derive(Model)]` for an enum whose variants carry fields.
//!
//! A value is a map: the variant's name under `$variant`
//! (`tilo_resource::VARIANT_KEY`), and each of the variant's fields under a
//! key of its own (`tilo_resource::model::field_key`), stored as its type
//! stores it.
//! Concurrent edits of one variant merge field by field (two people editing a
//! text field of the same variant splice it); changing the variant is one
//! last-writer-wins write of its name. A field belongs to the variant that
//! stored it: another variant naming a field alike — with another type, even —
//! never reads it, and what a variant stored stays when the value leaves it,
//! so an undo of the change reads its fields again.
//!
//! Named fields keep their names (`#[model(rename = "key")]` changes one,
//! `#[model(skip)]` leaves one out: its `Default` on read); the fields of a
//! tuple variant are stored as `0`, `1`, …. A unit variant is the map with its
//! name alone. The variant marked `#[default]` is the value of an absent slot,
//! when each of its fields has an empty value.

use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{DataEnum, DeriveInput, Fields, Ident, Type};

use crate::{field_options, krate, snake_case};

struct Field {
    /// How the variant's pattern and constructor name it: an identifier or
    /// a tuple index.
    member: syn::Member,
    /// The local the pattern binds it to.
    binding: Ident,
    key: String,
    ty: Type,
    skip: bool,
}

struct Variant {
    ident: Ident,
    key: String,
    fields: Vec<Field>,
    style: Style,
    default: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Unit,
    Named,
    Tuple,
}

fn variants(data: &DataEnum) -> syn::Result<Vec<Variant>> {
    let mut out = Vec::new();
    for variant in &data.variants {
        let options = field_options(&variant.attrs)?;
        let key = options
            .rename
            .unwrap_or_else(|| snake_case(&variant.ident.to_string()));
        let default = variant
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("default"));
        let style = match &variant.fields {
            Fields::Unit => Style::Unit,
            Fields::Named(_) => Style::Named,
            Fields::Unnamed(_) => Style::Tuple,
        };
        let mut fields = Vec::new();
        for (index, field) in variant.fields.iter().enumerate() {
            let options = field_options(&field.attrs)?;
            if options.label || options.readonly || options.unit || options.resource {
                return Err(syn::Error::new(
                    field.span(),
                    "a variant's field takes `rename`, `skip`, `atom` or `whole`",
                ));
            }
            let (member, name) = match &field.ident {
                Some(ident) => (syn::Member::Named(ident.clone()), ident.to_string()),
                None => (
                    syn::Member::Unnamed(syn::Index::from(index)),
                    index.to_string(),
                ),
            };
            let key = options.rename.unwrap_or_else(|| {
                name.strip_prefix("r#")
                    .map_or_else(|| name.clone(), str::to_owned)
            });
            if key == tilo_variant_key() {
                return Err(syn::Error::new(
                    field.span(),
                    "`$variant` names the variant; store this field under another key",
                ));
            }
            if key.starts_with('$') {
                return Err(syn::Error::new(
                    field.span(),
                    "a field's key may not begin with `$`: those name where a variant's fields are stored",
                ));
            }
            fields.push(Field {
                member,
                binding: format_ident!("__field{}", index),
                key,
                ty: field.ty.clone(),
                skip: options.skip,
            });
        }
        out.push(Variant {
            ident: variant.ident.clone(),
            key,
            fields,
            style,
            default,
        });
    }
    if out.iter().filter(|variant| variant.default).count() > 1 {
        return Err(syn::Error::new(
            Span::call_site(),
            "one variant at most is `#[default]`",
        ));
    }
    Ok(out)
}

/// `tilo_resource::VARIANT_KEY`, which a field may not be stored under.
const fn tilo_variant_key() -> &'static str {
    "$variant"
}

impl Variant {
    /// The pattern binding every field: `Self::V { a: __field0, .. }`.
    fn pattern(&self) -> Tokens {
        let ident = &self.ident;
        let members = self.fields.iter().map(|field| &field.member);
        let bindings = self.fields.iter().map(|field| &field.binding);
        match self.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members: #bindings),* }),
        }
    }

    /// The variant built from `value(field)` for each field.
    fn construct(&self, value: impl Fn(&Field) -> Tokens) -> Tokens {
        let ident = &self.ident;
        let members = self.fields.iter().map(|field| &field.member);
        let values = self.fields.iter().map(value);
        match self.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members: #values),* }),
        }
    }

    fn stored(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|field| !field.skip)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear expansion: each Model function over every variant"
)]
pub(crate) fn derive(input: &DeriveInput, data: &DataEnum) -> syn::Result<Tokens> {
    let krate = krate();
    let model = quote!(#krate::model);
    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let variants = variants(data)?;
    if variants.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "an enum with no variants holds no value",
        ));
    }

    let field_types: Vec<&Type> = variants
        .iter()
        .flat_map(|variant| variant.stored().map(|field| &field.ty))
        .collect();

    let to_json = variants.iter().map(|variant| {
        let pattern = variant.pattern();
        let key = &variant.key;
        let keys = variant.stored().map(|field| &field.key);
        let bindings = variant.stored().map(|field| &field.binding);
        let unused = variant
            .fields
            .iter()
            .filter(|field| field.skip)
            .map(|field| &field.binding);
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                #model::json_of_variant(
                    #key,
                    ::std::vec![#((#keys, #krate::Model::to_json(#bindings))),*],
                )
            }
        }
    });

    let from_json = variants.iter().map(|variant| {
        let key = &variant.key;
        let built = variant.construct(|field| {
            if field.skip {
                quote!(::core::default::Default::default())
            } else {
                let field_key = &field.key;
                quote!(#model::json_field(__fields, #field_key)?)
            }
        });
        quote!(#key => ::core::result::Result::Ok(#built),)
    });

    let load = variants.iter().map(|variant| {
        let key = &variant.key;
        let keys: Vec<&String> = variant.stored().map(|field| &field.key).collect();
        let read = variant.construct(|field| {
            if field.skip {
                quote!(::core::default::Default::default())
            } else {
                let field_key = &field.key;
                let ty = &field.ty;
                quote!(__fields.read::<#ty>(#field_key)?)
            }
        });
        quote! {
            #key => {
                let __fields = #model::variant_fields(doc, value, id, #key, &[#(#keys),*])?;
                ::core::result::Result::Ok(#read)
            }
        }
    });

    let store = variants.iter().map(|variant| {
        let pattern = variant.pattern();
        let key = &variant.key;
        let keys: Vec<&String> = variant.stored().map(|field| &field.key).collect();
        let bindings = variant.stored().map(|field| &field.binding);
        let field_keys = variant.stored().map(|field| &field.key);
        let unused = variant
            .fields
            .iter()
            .filter(|field| field.skip)
            .map(|field| &field.binding);
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                let __obj = #model::variant_map(doc, slot, #key, &[#(#keys),*])?;
                #(#model::write_variant_key(doc, &__obj, #key, #field_keys, #bindings)?;)*
                ::core::result::Result::Ok(())
            }
        }
    });

    let schemas = variants.iter().map(|variant| {
        let key = &variant.key;
        let keys = variant.stored().map(|field| &field.key);
        let types = variant.stored().map(|field| &field.ty);
        quote! {
            #model::variant_schema(
                #key,
                ::std::vec![#((#keys, <#types as #krate::Model>::value_schema())),*],
            )
        }
    });

    let checks = variants.iter().map(|variant| {
        let pattern = variant.pattern();
        let checked = variant.stored().map(|field| &field.binding);
        let unused = variant
            .fields
            .iter()
            .filter(|field| field.skip)
            .map(|field| &field.binding);
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                #(#krate::Model::check(#checked)?;)*
                ::core::result::Result::Ok(())
            }
        }
    });

    let texts = variants.iter().map(|variant| {
        let pattern = variant.pattern();
        let collected = variant.stored().map(|field| &field.binding);
        let unused = variant
            .fields
            .iter()
            .filter(|field| field.skip)
            .map(|field| &field.binding);
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                #(#krate::Model::collect_text(#collected, out);)*
            }
        }
    });

    let missing = variants.iter().find(|variant| variant.default).map_or_else(
        || {
            quote! {
                ::core::result::Result::Err(#krate::ModelError::Missing {
                    path: ::std::string::String::new(),
                })
            }
        },
        |variant| {
            let built = variant.construct(|field| {
                if field.skip {
                    quote!(::core::default::Default::default())
                } else {
                    let ty = &field.ty;
                    quote!(<#ty as #krate::Model>::missing()?)
                }
            });
            quote!(::core::result::Result::Ok(#built))
        },
    );

    Ok(quote! {
        impl #impl_generics #krate::Settable for #name #type_generics #where_clause {}

        impl #impl_generics #krate::Model for #name #type_generics #where_clause {
            const CHECKS: bool = false #(|| <#field_types as #krate::Model>::CHECKS)*;
            type Paths<__R> = #krate::Path<__R, Self>;

            fn paths_at<__R>(path: #krate::Path<__R, Self>) -> #krate::Path<__R, Self> {
                path
            }

            fn to_json(&self) -> #krate::__private::serde_json::Value {
                match self {
                    #(#to_json)*
                }
            }

            fn from_json(
                value: &#krate::__private::serde_json::Value,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let (__variant, __fields) = #model::json_variant(value)?;
                let _ = __fields;
                match __variant {
                    #(#from_json)*
                    __other => ::core::result::Result::Err(#model::unknown_variant(__other)),
                }
            }

            fn load(
                doc: &#krate::Doc,
                value: &#krate::__private::Value<'_>,
                id: &#krate::__private::ObjId,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let __variant = #model::stored_variant(doc, value, id)?;
                match __variant.as_str() {
                    #(#load)*
                    __other => ::core::result::Result::Err(#model::unknown_variant(__other)),
                }
            }

            fn missing() -> ::core::result::Result<Self, #krate::ModelError> {
                #missing
            }

            fn store(
                &self,
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                match self {
                    #(#store)*
                }
            }

            fn value_schema() -> #krate::__private::serde_json::Value {
                #krate::schema::one_of(::std::vec![#(#schemas),*])
            }

            fn check(&self) -> ::core::result::Result<(), #krate::Refusal> {
                match self {
                    #(#checks)*
                }
            }

            fn collect_text(&self, out: &mut ::std::vec::Vec<::std::string::String>) {
                match self {
                    #(#texts)*
                }
            }
        }
    })
}
