//! `#[derive(Model)]` for an enum whose variants carry fields.
//!
//! A value is a map: the variant's name under `$variant`
//! (`tilo_resource::VARIANT_KEY`), and each of the variant's fields under a
//! key of its own (`tilo_resource::model::field_key`), stored as its type
//! stores it. Concurrent edits of one variant merge field by field (two
//! people editing a text field of the same variant splice it); changing the
//! variant is one last-writer-wins write of its name.
//!
//! A field path names its variant before the field (`shape.rect.x`), so
//! fields of different variants never share a path even when they share a
//! storage key. A field belongs to the variant that stored it: another
//! variant naming a field alike — with another type, even — never reads it,
//! and what a variant stored stays when the value leaves it, so an undo of
//! the change reads its fields again.
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

use crate::{doc_lines, field_options, humanize, krate, snake_case};

struct Field {
    /// How the variant's pattern and constructor name it: an identifier or
    /// a tuple index.
    member: syn::Member,
    /// The field's Rust name.
    name: String,
    key: String,
    ty: Type,
    skip: bool,
    atom: bool,
    whole: bool,
    title: String,
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
        let default = options.default_variant
            || variant
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
            if options.label
                || options.readonly
                || options.unit
                || options.resource
                || options.keyed
            {
                return Err(syn::Error::new(
                    field.span(),
                    "a variant's field takes `rename`, `skip`, `default`, `atom` or `whole`",
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
            let title = doc_lines(&field.attrs)
                .into_iter()
                .find(|line| !line.is_empty())
                .map_or_else(
                    || humanize(&key),
                    |line| line.trim_end_matches('.').to_owned(),
                );
            if key.starts_with('$') {
                return Err(syn::Error::new(
                    field.span(),
                    "a field's key may not begin with `$`: those name where a variant's fields are stored",
                ));
            }
            fields.push(Field {
                member,
                name,
                key,
                ty: field.ty.clone(),
                skip: options.skip,
                atom: options.atom,
                whole: options.whole,
                title,
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

fn ty_text(ty: &Type) -> String {
    quote!(#ty)
        .to_string()
        .replace(" < ", "<")
        .replace("< ", "<")
        .replace(" <", "<")
        .replace(" >", ">")
        .replace(" :: ", "::")
        .replace(":: ", "::")
        .replace(" ,", ",")
}

fn binding_of(index: usize) -> Ident {
    format_ident!("__field{}", index)
}

impl Variant {
    /// The pattern binding every field with `prefix` before its position.
    fn pattern_with(&self, prefix: &str) -> Tokens {
        let ident = &self.ident;
        let members = self.fields.iter().map(|field| &field.member);
        let bindings: Vec<Ident> = self
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("{}{}", prefix, index))
            .collect();
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
    let table = quote!(#krate::table);
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
    let owner = snake_case(&name.to_string()).replace('_', "-");

    let field_types: Vec<&Type> = variants
        .iter()
        .flat_map(|variant| variant.stored().map(|field| &field.ty))
        .collect();

    let table_expr = |field: &Field| {
        if field.atom {
            quote!(&#table::ATOM_STRING)
        } else {
            let ty = &field.ty;
            quote!(<#ty as #krate::Model>::TABLE)
        }
    };

    let variant_descs = variants.iter().map(|variant| {
        let variant_key = &variant.key;
        let variant_name = variant.ident.to_string();
        let fields = variant.stored().map(|field| {
            let key = &field.key;
            let name = &field.name;
            let field_table = table_expr(field);
            let whole = field.whole;
            quote!(#table::FieldDesc {
                key: #key,
                variant: ::core::option::Option::Some(#variant_key),
                name: #name,
                table: #field_table,
                readonly: false,
                whole: #whole,
                items: ::core::option::Option::None,
            })
        });
        quote!(#table::VariantDesc {
            key: #variant_key,
            name: #variant_name,
            fields: &[#(#fields),*],
        })
    });

    let variant_shapes = variants.iter().map(|variant| {
        let key = &variant.key;
        let variant_name = variant.ident.to_string();
        let fields = variant.stored().map(|field| {
            let key = &field.key;
            let name = &field.name;
            let ty = &field.ty;
            let ty_text = ty_text(ty);
            let title = &field.title;
            let owner = &owner;
            quote!(#krate::FieldShape {
                key: #key,
                owner: #owner,
                name: #name,
                ty: #ty_text,
                title: #title,
                shape: <#ty as #krate::Model>::SHAPE,
                unit: false,
                readonly: false,
                label: false,
                label_part: 0,
                quiet_remove: false,
                trust: #krate::Trust::Ordinary,
                schema: <#ty as #krate::Model>::value_schema,
                text: #krate::stored_text::<#ty>,
                absent: #krate::absent_scalar::<#ty>,
                resource: ::core::option::Option::None,
            })
        });
        quote!(#krate::VariantShape {
            key: #key,
            name: #variant_name,
            fields: &[#(#fields),*],
        })
    });

    let to_json = variants.iter().map(|variant| {
        let pattern = variant.pattern_with("__field");
        let key = &variant.key;
        let keys = variant.stored().map(|field| &field.key);
        let bindings: Vec<Ident> = variant
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| !field.skip)
            .map(|(index, _)| binding_of(index))
            .collect();
        let unused: Vec<Ident> = variant
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.skip)
            .map(|(index, _)| binding_of(index))
            .collect();
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

    let load = variants.iter().enumerate().map(|(variant_index, variant)| {
        let key = &variant.key;
        let read = variant.construct(|field| {
            if field.skip {
                quote!(::core::default::Default::default())
            } else {
                let field_index = variant
                    .stored()
                    .position(|candidate| ::core::ptr::eq(candidate, field))
                    .unwrap_or_default();
                let ty = &field.ty;
                quote!({
                    let mut __out: ::core::option::Option<#ty> = ::core::option::Option::None;
                    __fields.read_field(
                        &Self::__VARIANTS[#variant_index].fields[#field_index],
                        &mut __out,
                    )?;
                    #table::filled(__out)?
                })
            }
        });
        quote! {
            #key => {
                let __fields = #model::variant_fields(
                    doc,
                    value,
                    id,
                    #key,
                    Self::__VARIANTS[#variant_index].fields,
                )?;
                let mut __value = #read;
                #krate::Model::read_whole(&mut __value, doc)?;
                ::core::result::Result::Ok(__value)
            }
        }
    });

    let store = variants.iter().enumerate().map(|(variant_index, variant)| {
        let pattern = variant.pattern_with("__field");
        let key = &variant.key;
        let keys: Vec<&String> = variant.stored().map(|field| &field.key).collect();
        let writes = variant.stored().enumerate().map(|(field_index, field)| {
            let binding = binding_of(
                variant
                    .fields
                    .iter()
                    .position(|candidate| ::core::ptr::eq(candidate, field))
                    .unwrap_or_default(),
            );
            quote!(#table::store_field(
                &Self::__VARIANTS[#variant_index].fields[#field_index],
                doc,
                &__obj,
                #binding,
            )?;)
        });
        let unused: Vec<Ident> = variant
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.skip)
            .map(|(index, _)| binding_of(index))
            .collect();
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                let __obj = #model::variant_map(doc, slot, #key, &[#(#keys),*])?;
                #(#writes)*
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
        let pattern = variant.pattern_with("__field");
        let checked = variant.stored().map(|field| {
            binding_of(
                variant
                    .fields
                    .iter()
                    .position(|candidate| ::core::ptr::eq(candidate, field))
                    .unwrap_or_default(),
            )
        });
        let unused: Vec<Ident> = variant
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.skip)
            .map(|(index, _)| binding_of(index))
            .collect();
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                #(#krate::Model::check(#checked)?;)*
                ::core::result::Result::Ok(())
            }
        }
    });

    let texts = variants.iter().map(|variant| {
        let pattern = variant.pattern_with("__field");
        let collected = variant.stored().map(|field| {
            binding_of(
                variant
                    .fields
                    .iter()
                    .position(|candidate| ::core::ptr::eq(candidate, field))
                    .unwrap_or_default(),
            )
        });
        let unused: Vec<Ident> = variant
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.skip)
            .map(|(index, _)| binding_of(index))
            .collect();
        quote! {
            #pattern => {
                #(let _ = #unused;)*
                #(#krate::Model::collect_text(#collected, out);)*
            }
        }
    });

    let misses = variants.iter().find(|variant| variant.default).map_or_else(
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

    let stores_as = variants.iter().enumerate().map(|(variant_index, variant)| {
        let ident = &variant.ident;
        let members_now = variant.fields.iter().map(|field| &field.member);
        let members_was = variant.fields.iter().map(|field| &field.member);
        let now_bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__now{}", index));
        let was_bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__was{}", index));
        let pattern_now = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => {
                quote!(Self::#ident { #(#members_now: #now_bindings),* })
            }
        };
        let pattern_was = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => {
                quote!(Self::#ident { #(#members_was: #was_bindings),* })
            }
        };
        let comparisons: Vec<Tokens> = variant
            .stored()
            .enumerate()
            .map(|(field_index, field)| {
                let index = variant
                    .fields
                    .iter()
                    .position(|candidate| ::core::ptr::eq(candidate, field))
                    .unwrap_or_default();
                let now = format_ident!("__now{}", index);
                let was = format_ident!("__was{}", index);
                quote!(#table::stores_as_field(
                    Self::__VARIANTS[#variant_index].fields[#field_index].table,
                    #now,
                    #was,
                ))
            })
            .collect();
        let result = if comparisons.is_empty() {
            quote!(true)
        } else {
            quote!(#(#comparisons)&&*)
        };
        quote! {
            (#pattern_now, #pattern_was) => #result,
        }
    });

    let diffs = variants.iter().enumerate().map(|(variant_index, variant)| {
        let ident = &variant.ident;
        let members_now = variant.fields.iter().map(|field| &field.member);
        let members_was = variant.fields.iter().map(|field| &field.member);
        let now_bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__now{}", index));
        let was_bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__was{}", index));
        let pattern_now = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => {
                quote!(Self::#ident { #(#members_now: #now_bindings),* })
            }
        };
        let pattern_was = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => {
                quote!(Self::#ident { #(#members_was: #was_bindings),* })
            }
        };
        let fields = variant.stored().enumerate().map(|(field_index, field)| {
            let index = variant
                .fields
                .iter()
                .position(|candidate| ::core::ptr::eq(candidate, field))
                .unwrap_or_default();
            let now = format_ident!("__now{}", index);
            let was = format_ident!("__was{}", index);
            let key = &variant.key;
            let field_key = &field.key;
            quote! {
                if !#table::stores_as_field(
                    Self::__VARIANTS[#variant_index].fields[#field_index].table,
                    #now,
                    #was,
                ) {
                    let mut __at = path.to_vec();
                    __at.push(#krate::Segment::Field(::std::string::String::from(#key)));
                    __at.push(#krate::Segment::Field(::std::string::String::from(#field_key)));
                    #table::diff_field(
                        Self::__VARIANTS[#variant_index].fields[#field_index].table,
                        #was,
                        #now,
                        &__at,
                        &mut found,
                    );
                }
            }
        });
        quote! {
            (#pattern_was, #pattern_now) => {
                #(#fields)*
            }
        }
    });

    let store_changed = variants.iter().enumerate().map(|(variant_index, variant)| {
        let ident = &variant.ident;
        let members_now = variant.fields.iter().map(|field| &field.member);
        let members_was = variant.fields.iter().map(|field| &field.member);
        let now_bindings = variant.fields.iter().enumerate().map(|(index, _)| {
            format_ident!("__now{}", index)
        });
        let was_bindings = variant.fields.iter().enumerate().map(|(index, _)| {
            format_ident!("__was{}", index)
        });
        let pattern_now = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members_now: #now_bindings),* }),
        };
        let pattern_was = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members_was: #was_bindings),* }),
        };
        let key = &variant.key;
        let writes = variant.stored().enumerate().map(|(field_index, field)| {
            let index = variant
                .fields
                .iter()
                .position(|candidate| ::core::ptr::eq(candidate, field))
                .unwrap_or_default();
            let now = format_ident!("__now{}", index);
            let was = format_ident!("__was{}", index);
            quote! {
                {
                    let __field = &Self::__VARIANTS[#variant_index].fields[#field_index];
                    let __storage = __field.storage_key();
                    #table::write_key_changed_at(
                        __field.table,
                        (#now, #was),
                        doc,
                        &__obj,
                        __field.key,
                        __storage.as_ref(),
                        dirty,
                    )?;
                }
            }
        });
        quote! {
            (#pattern_now, #pattern_was) => {
                if #model::slot_variant(doc, slot)?.as_deref() != ::core::option::Option::Some(#key) {
                    return self.store(doc, slot);
                }
                let __obj = match slot.current(doc)? {
                    ::core::option::Option::Some((
                        #krate::__private::Value::Object(#krate::__private::ObjType::Map),
                        id,
                    )) => id,
                    _ => return self.store(doc, slot),
                };
                // A document of the older flat layout stores every field
                // under its variant once, as a whole write does; writing only
                // the changed fields would leave flat keys beside them.
                if !#model::variant_stored(doc, &__obj, #key)? {
                    return self.store(doc, slot);
                }
                #(#writes)*
                ::core::result::Result::Ok(())
            }
        }
    });

    let get_at = variants.iter().enumerate().map(|(variant_index, variant)| {
        let ident = &variant.ident;
        let members = variant.fields.iter().map(|field| &field.member);
        let bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__field{}", index));
        let pattern = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members: #bindings),* }),
        };
        let lookups = variant.stored().enumerate().map(|(field_index, field)| {
            let index = variant
                .fields
                .iter()
                .position(|candidate| ::core::ptr::eq(candidate, field))
                .unwrap_or_default();
            let binding = format_ident!("__field{}", index);
            let field_key = &field.key;
            quote! {
                if __key == #field_key {
                    let __field = &Self::__VARIANTS[#variant_index].fields[#field_index];
                    return #table::field_at(__field.table, #binding, __rest);
                }
            }
        });
        quote! {
            #pattern => {
                #(#lookups)*
                ::core::option::Option::None
            }
        }
    });

    let field_refs = variants
        .iter()
        .enumerate()
        .flat_map(|(variant_index, variant)| {
            let ident = &variant.ident;
            let members = variant.fields.iter().map(|field| &field.member);
            let bindings = variant
                .fields
                .iter()
                .enumerate()
                .map(|(index, _)| format_ident!("__field{}", index));
            let pattern = match variant.style {
                Style::Unit => quote!(Self::#ident),
                Style::Named | Style::Tuple => quote!(Self::#ident { #(#members: #bindings),* }),
            };
            variant
                .stored()
                .enumerate()
                .map(move |(field_index, field)| {
                    let index = variant
                        .fields
                        .iter()
                        .position(|candidate| ::core::ptr::eq(candidate, field))
                        .unwrap_or_default();
                    let binding = format_ident!("__field{}", index);
                    let variant_index = proc_macro2::Literal::usize_unsuffixed(variant_index);
                    let field_index = proc_macro2::Literal::usize_unsuffixed(field_index);
                    quote! {
                        (#variant_index, #field_index) => match self {
                            #pattern => ::core::option::Option::Some(#binding),
                            _ => ::core::option::Option::None,
                        },
                    }
                })
        });

    let read_whole = variants.iter().map(|variant| {
        let ident = &variant.ident;
        let members = variant.fields.iter().map(|field| &field.member);
        let bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__field{}", index));
        let pattern = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members: #bindings),* }),
        };
        let reads = variant.stored().filter(|field| field.whole).map(|field| {
            let index = variant
                .fields
                .iter()
                .position(|candidate| ::core::ptr::eq(candidate, field))
                .unwrap_or_default();
            let binding = format_ident!("__field{}", index);
            quote!(#krate::Model::read_whole(#binding, doc)?;)
        });
        quote! {
            #pattern => {
                #(#reads)*
            }
        }
    });

    let nested = variants.iter().map(|variant| {
        let ident = &variant.ident;
        let members = variant.fields.iter().map(|field| &field.member);
        let bindings = variant
            .fields
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__field{}", index));
        let pattern = match variant.style {
            Style::Unit => quote!(Self::#ident),
            Style::Named | Style::Tuple => quote!(Self::#ident { #(#members: #bindings),* }),
        };
        let items = variant.stored().map(|field| {
            let index = variant
                .fields
                .iter()
                .position(|candidate| ::core::ptr::eq(candidate, field))
                .unwrap_or_default();
            let binding = format_ident!("__field{}", index);
            quote!(found.extend(#krate::Model::nested_collections(#binding));)
        });
        quote! {
            #pattern => {
                #(#items)*
            }
        }
    });

    let variant_named = variants.iter().enumerate().map(|(index, variant)| {
        let key = &variant.key;
        let index = proc_macro2::Literal::usize_unsuffixed(index);
        quote!(#key => ::core::option::Option::Some(#index),)
    });

    let variant_index = variants.iter().enumerate().map(|(index, variant)| {
        let ident = &variant.ident;
        let index = proc_macro2::Literal::usize_unsuffixed(index);
        quote!(Self::#ident { .. } => ::core::option::Option::Some(#index),)
    });

    let default_variant = variants
        .iter()
        .position(|variant| variant.default)
        .map_or_else(
            || quote!(::core::option::Option::None),
            |index| {
                let index = proc_macro2::Literal::usize_unsuffixed(index);
                quote!(::core::option::Option::Some(#index))
            },
        );

    let edit_field = variants.iter().enumerate().map(|(variant_index, variant)| {
        let key = &variant.key;
        let keys: Vec<&String> = variant.stored().map(|field| &field.key).collect();
        let fields = variant.stored().enumerate().map(|(field_index, field)| {
            let field_key = &field.key;
            let ty = &field.ty;
            let whole = field.whole;
            quote! {
                if __field_key == #field_key {
                    let __field = &Self::__VARIANTS[#variant_index].fields[#field_index];
                    let __storage = __field.storage_key();
                    let __obj = match slot.current(doc)? {
                        ::core::option::Option::Some((
                            #krate::__private::Value::Object(#krate::__private::ObjType::Map),
                            id,
                        )) => id,
                        ::core::option::Option::Some(_) => {
                            return ::core::result::Result::Err(#krate::ModelError::shape(
                                "a map naming its variant",
                            ));
                        }
                        ::core::option::Option::None => {
                            #model::variant_map(doc, slot, #key, &[#(#keys),*])?
                        }
                    };
                    // An older version stored this field flat, beside the
                    // variant's name. Read it as it was written, write it by
                    // its variant, and leave the flat key gone; the edit then
                    // lands on the field stored by variant.
                    if !#model::variant_stored(doc, &__obj, #key)? {
                        if let ::core::option::Option::Some((__legacy, __legacy_id)) =
                            #krate::Slot::Key(&__obj, __field.key).current(doc)?
                        {
                            let mut __loaded: #ty = #table::Run(__field.table)
                                .load(doc, &__legacy, &__legacy_id)?;
                            if #whole {
                                #krate::Model::read_whole(&mut __loaded, doc)?;
                            }
                            #table::store_field(__field, doc, &__obj, &__loaded)?;
                            #model::delete_key(doc, &__obj, __field.key)?;
                        }
                    }
                    #krate::LocatedAt::located(
                        #table::edit_field(
                            __field.table,
                            doc,
                            #krate::Slot::Key(&__obj, __storage.as_ref()),
                            __below,
                            op,
                            cx,
                        ),
                        __field.key,
                    )?;
                    let __value = #krate::__private::Value::Object(#krate::__private::ObjType::Map);
                    let __loaded = <Self as #krate::Model>::load(doc, &__value, &__obj)?;
                    <Self as #krate::Model>::check(&__loaded)
                        .map_err(#krate::ModelError::refused)?;
                    return ::core::result::Result::Ok(());
                }
            }
        });
        quote! {
            #key => {
                #(#fields)*
                let names: ::std::vec::Vec<&str> = Self::__VARIANTS[#variant_index]
                    .fields
                    .iter()
                    .map(|field| field.key)
                    .collect();
                return ::core::result::Result::Err(#krate::ModelError::invalid(format!(
                    "no field `{}` in variant `{}`; its fields are {}",
                    __field_key,
                    #key,
                    names.join(", "),
                )));
            }
        }
    });

    // Typed paths: one field per variant (`shape.rect`), and inside it one
    // per stored field (`shape.rect.x`).
    Ok(quote! {
        impl #impl_generics #name #type_generics #where_clause {
            /// The data enum as data: its variants' names and fields.
            #[doc(hidden)]
            pub const __VARIANTS: &'static [#table::VariantDesc] = &[#(#variant_descs),*];

            #[doc(hidden)]
            pub fn __variant_named(name: &str) -> ::core::option::Option<usize> {
                match name {
                    #(#variant_named)*
                    _ => ::core::option::Option::None,
                }
            }

            #[doc(hidden)]
            pub fn __variant_index(value: &dyn ::core::any::Any) -> ::core::option::Option<usize> {
                let value = value.downcast_ref::<Self>()?;
                match value {
                    #(#variant_index)*
                }
            }

            #[doc(hidden)]
            pub fn __default_variant() -> ::core::option::Option<usize> {
                #default_variant
            }

            #[doc(hidden)]
            pub fn __field_ref(
                &self,
                variant: usize,
                field: usize,
            ) -> ::core::option::Option<&dyn ::core::any::Any> {
                match (variant, field) {
                    #(#field_refs)*
                    _ => ::core::option::Option::None,
                }
            }

            /// Edit `field_key` of variant `variant_key`, whose field is
            /// `below` the new path.
            #[doc(hidden)]
            pub fn __edit_field(
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
                __key: &str,
                __field_key: &str,
                __below: &[#krate::Segment],
                op: &#krate::EditOp,
                cx: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                match __key {
                    #(#edit_field)*
                    _ => ::core::result::Result::Err(#model::unknown_variant(__key)),
                }
            }
        }

        impl #impl_generics #krate::Settable for #name #type_generics #where_clause {}

        impl #impl_generics #krate::Model for #name #type_generics #where_clause {
            const SHAPE: #krate::Shape = #krate::Shape::Variant(&[#(#variant_shapes),*]);
            const CHECKS: bool = false #(|| <#field_types as #krate::Model>::CHECKS)*;
            type Paths<__R> = #krate::Path<__R, Self>;

            fn paths_at<__R>(path: #krate::Path<__R, Self>) -> Self::Paths<__R> {
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
                #misses
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

            fn stores_as(&self, before: &Self) -> bool {
                match (self, before) {
                    #(#stores_as)*
                    _ => false,
                }
            }

            fn store_changed(
                &self,
                before: &Self,
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
                dirty: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                if self.stores_as(before) {
                    return ::core::result::Result::Ok(());
                }
                match (self, before) {
                    #(#store_changed)*
                    _ => self.store(doc, slot),
                }
            }

            fn diff<R>(
                &self,
                after: &Self,
                path: &[#krate::Segment],
                out: &mut ::std::vec::Vec<#krate::Edit<R>>,
            ) {
                if self.stores_as(after) {
                    return;
                }
                let mut found = ::std::vec::Vec::new();
                match (self, after) {
                    #(#diffs)*
                    _ => found.push(#krate::Edit::new(
                        path.to_vec(),
                        #krate::EditOp::Set { value: after.to_json() },
                    )),
                }
                for edit in found {
                    out.push(edit.retyped());
                }
            }

            fn edit(
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
                path: &[#krate::Segment],
                op: &#krate::EditOp,
                cx: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let Some((#krate::Segment::Field(__first), __rest)) = path.split_first() else {
                    return #krate::edit_whole::<Self>(doc, slot, path, op);
                };
                // A variant names the branch; otherwise the active variant's
                // field is named directly.
                if let ::core::option::Option::Some(__variant_index) = Self::__variant_named(__first)
                {
                    let __key = Self::__VARIANTS[__variant_index].key;
                    if __rest.is_empty() {
                        return ::core::result::Result::Err(#krate::ModelError::invalid(
                            "a variant is changed by setting the whole value",
                        ));
                    }
                    let Some((#krate::Segment::Field(__field_key), __below)) = __rest.split_first() else {
                        return ::core::result::Result::Err(#krate::ModelError::unsupported(op));
                    };
                    return Self::__edit_field(doc, slot, __key, __field_key, __below, op, cx);
                }
                let __field_key = __first;
                let __below = __rest;
                let __current = #model::slot_variant(doc, slot)?;
                let __variant_index = __current
                    .as_deref()
                    .and_then(Self::__variant_named)
                    .or_else(Self::__default_variant)
                    .ok_or_else(|| #krate::ModelError::Missing {
                        path: ::std::string::String::new(),
                    })?;
                let __key = Self::__VARIANTS[__variant_index].key;
                Self::__edit_field(doc, slot, __key, __field_key, __below, op, cx)
            }

            fn get_at(&self, path: &[#krate::Segment]) -> ::core::option::Option<&dyn ::core::any::Any> {
                if path.is_empty() {
                    return ::core::option::Option::Some(self);
                }
                let Some((#krate::Segment::Field(__first), __rest)) = path.split_first() else {
                    return ::core::option::Option::None;
                };
                if let ::core::option::Option::Some(__variant_index) = Self::__variant_named(__first)
                {
                    if Self::__variant_index(self) != ::core::option::Option::Some(__variant_index) {
                        return ::core::option::Option::None;
                    }
                    if __rest.is_empty() {
                        return ::core::option::Option::Some(self);
                    }
                    let Some((#krate::Segment::Field(__field_key), __below)) = __rest.split_first() else {
                        return ::core::option::Option::None;
                    };
                    let __fields = Self::__VARIANTS[__variant_index].fields;
                    let (__field_index, __field) = __fields
                        .iter()
                        .enumerate()
                        .find(|(_, field)| field.key == __field_key.as_str())?;
                    let __value = self.__field_ref(__variant_index, __field_index)?;
                    return #table::field_at(__field.table, __value, __below);
                }
                let __key = __first;
                match self {
                    #(#get_at)*
                    _ => ::core::option::Option::None,
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

            fn read_whole(
                &mut self,
                doc: &#krate::Doc,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let _ = doc;
                match self {
                    #(#read_whole)*
                }
                ::core::result::Result::Ok(())
            }

            fn nested_collections(&self) -> ::std::vec::Vec<#krate::items::ItemCollection> {
                let mut found = ::std::vec::Vec::new();
                match self {
                    #(#nested)*
                }
                found
            }
        }
    })
}
