//! `#[derive(JsonSchema)]`: the schema of a type's serde JSON form, read
//! from the type's definition and the serde attributes that shape it.

use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Attribute, Data, DeriveInput, Fields, Ident, Token};

use crate::krate;

/// How an enum's variants are told apart in JSON.
enum Tagging {
    External,
    Internal(String),
    Adjacent(String, String),
}

#[derive(Default)]
struct ContainerOptions {
    rename_all: Option<String>,
    tag: Option<String>,
    content: Option<String>,
    transparent: bool,
    default: bool,
}

#[derive(Default)]
struct ItemOptions {
    rename: Option<String>,
    rename_all: Option<String>,
    default: bool,
    skip: bool,
}

/// Consume the value of a serde option this derive has no use for, so the
/// rest of the attribute still parses.
fn ignore(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(Token![=]) {
        let _: syn::Expr = meta.value()?.parse()?;
    } else if meta.input.peek(syn::token::Paren) {
        let _: proc_macro2::Group = meta.input.parse()?;
    }
    Ok(())
}

fn unsupported(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Error {
    let name = meta
        .path
        .get_ident()
        .map_or_else(|| "this option".to_owned(), |ident| format!("`{ident}`"));
    meta.error(format!(
        "JsonSchema cannot describe serde's {name}; the schema would not match what serde reads"
    ))
}

fn string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    if meta.input.peek(syn::token::Paren) {
        return Err(meta
            .error("JsonSchema needs one spelling for reading and writing; use `name = \"...\"`"));
    }
    Ok(meta.value()?.parse::<syn::LitStr>()?.value())
}

fn serde_attrs(
    attrs: &[Attribute],
    mut each: impl FnMut(&syn::meta::ParseNestedMeta<'_>) -> syn::Result<()>,
) -> syn::Result<()> {
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| each(&meta))?;
    }
    Ok(())
}

fn container_options(attrs: &[Attribute]) -> syn::Result<ContainerOptions> {
    let mut options = ContainerOptions::default();
    serde_attrs(attrs, |meta| {
        let path = &meta.path;
        if path.is_ident("rename_all") {
            options.rename_all = Some(string_value(meta)?);
        } else if path.is_ident("tag") {
            options.tag = Some(string_value(meta)?);
        } else if path.is_ident("content") {
            options.content = Some(string_value(meta)?);
        } else if path.is_ident("transparent") {
            options.transparent = true;
        } else if path.is_ident("default") {
            options.default = true;
            ignore(meta)?;
        } else if path.is_ident("remote") && string_value(meta)? == "Self" {
            // The type's own derived code under another name: what serde
            // reads is still the type as written.
        } else if [
            "untagged",
            "from",
            "try_from",
            "into",
            "remote",
            "rename_all_fields",
            "variant_identifier",
            "field_identifier",
        ]
        .iter()
        .any(|name| path.is_ident(name))
        {
            return Err(unsupported(meta));
        } else {
            ignore(meta)?;
        }
        Ok(())
    })?;
    Ok(options)
}

fn item_options(attrs: &[Attribute], variant: bool) -> syn::Result<ItemOptions> {
    let mut options = ItemOptions::default();
    serde_attrs(attrs, |meta| {
        let path = &meta.path;
        if path.is_ident("rename") {
            options.rename = Some(string_value(meta)?);
        } else if variant && path.is_ident("rename_all") {
            options.rename_all = Some(string_value(meta)?);
        } else if path.is_ident("default") {
            options.default = true;
            ignore(meta)?;
        } else if path.is_ident("skip")
            || (variant
                && (path.is_ident("skip_deserializing") || path.is_ident("skip_serializing")))
        {
            options.skip = true;
        } else if [
            "flatten",
            "with",
            "serialize_with",
            "deserialize_with",
            "skip_deserializing",
            "other",
            "untagged",
        ]
        .iter()
        .any(|name| path.is_ident(name))
        {
            return Err(unsupported(meta));
        } else {
            ignore(meta)?;
        }
        Ok(())
    })?;
    Ok(options)
}

/// A field's `#[schema(range(min = …, max = …))]`: the bounds narrowing its
/// type's, either side optional.
#[derive(Default)]
struct Range {
    min: Option<syn::Expr>,
    max: Option<syn::Expr>,
}

fn field_range(attrs: &[Attribute]) -> syn::Result<Option<Range>> {
    let mut found = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("schema")) {
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("range") {
                return Err(meta.error("JsonSchema knows `#[schema(range(min = …, max = …))]`"));
            }
            let mut range = Range::default();
            meta.parse_nested_meta(|bound| {
                let value: syn::Expr = bound.value()?.parse()?;
                if bound.path.is_ident("min") {
                    range.min = Some(value);
                } else if bound.path.is_ident("max") {
                    range.max = Some(value);
                } else {
                    return Err(bound.error("a range has `min` and `max`"));
                }
                Ok(())
            })?;
            if range.min.is_none() && range.max.is_none() {
                return Err(meta.error("a range needs `min`, `max` or both"));
            }
            found = Some(range);
            Ok(())
        })?;
    }
    Ok(found)
}

/// Split an identifier into its lowercase words: `SpliceText` and
/// `splice_text` both become `["splice", "text"]`.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in name.chars() {
        if c == '_' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if c.is_uppercase() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.extend(c.to_lowercase());
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Serde's `rename_all` rules, applied to a variant (`PascalCase`) or a field
/// (`snake_case`) name.
fn rename(name: &str, rule: &str, span: proc_macro2::Span) -> syn::Result<String> {
    let parts = words(name);
    Ok(match rule {
        "lowercase" => name.to_lowercase(),
        "UPPERCASE" => name.to_uppercase(),
        "PascalCase" => parts.iter().map(|word| capitalized(word)).collect(),
        "camelCase" => parts
            .iter()
            .enumerate()
            .map(|(index, word)| {
                if index == 0 {
                    word.clone()
                } else {
                    capitalized(word)
                }
            })
            .collect(),
        "snake_case" => parts.join("_"),
        "SCREAMING_SNAKE_CASE" => parts.join("_").to_uppercase(),
        "kebab-case" => parts.join("-"),
        "SCREAMING-KEBAB-CASE" => parts.join("-").to_uppercase(),
        other => {
            return Err(syn::Error::new(
                span,
                format!("unknown serde rename rule `{other}`"),
            ));
        }
    })
}

/// The JSON name of a field or variant.
fn json_name(ident: &Ident, options: &ItemOptions, rule: Option<&String>) -> syn::Result<String> {
    if let Some(name) = &options.rename {
        return Ok(name.clone());
    }
    let name = ident.to_string();
    let name = name.strip_prefix("r#").unwrap_or(&name).to_owned();
    // Serde renames a variant from its `PascalCase` spelling and a field from
    // its `snake_case` one; `words` splits both, and the rules agree on
    // what each spelling becomes.
    match rule {
        Some(rule) => rename(&name, rule, ident.span()),
        None => Ok(name),
    }
}

/// Doc comments as one plain line: rustdoc links keep their text.
fn plain_doc(attrs: &[Attribute]) -> String {
    let joined = crate::doc_lines(attrs)
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::with_capacity(joined.len());
    let mut chars = joined.chars().peekable();
    // Brackets inside code (`[at, at+del)`) are text, not links.
    let mut in_code = false;
    while let Some(c) = chars.next() {
        match c {
            '`' => {
                in_code = !in_code;
                out.push(c);
            }
            '[' if !in_code => {}
            ']' if !in_code => {
                // `[text](target)`: drop the target.
                if chars.peek() == Some(&'(') {
                    let mut depth = 0_usize;
                    for next in chars.by_ref() {
                        match next {
                            '(' => depth = depth.saturating_add(1),
                            ')' => {
                                depth = depth.saturating_sub(1);
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// The table of a set of fields (a `schema::Content`): an object for named
/// fields, the inner schema for a newtype, a fixed-length array for a tuple,
/// `None` for a unit.
fn fields_schema(
    fields: &Fields,
    rule: Option<&String>,
    all_default: bool,
) -> syn::Result<Option<Tokens>> {
    let krate = krate();
    Ok(match fields {
        Fields::Named(named) => {
            let mut properties = Vec::new();
            for field in &named.named {
                let options = item_options(&field.attrs, false)?;
                if options.skip {
                    continue;
                }
                let Some(ident) = &field.ident else { continue };
                let name = json_name(ident, &options, rule)?;
                let ty = &field.ty;
                let doc = plain_doc(&field.attrs);
                let defaulted = options.default || all_default;
                let range = if let Some(Range { min, max }) = field_range(&field.attrs)? {
                    let bound = |value: Option<syn::Expr>| {
                        value.map_or_else(
                            || quote!(::core::option::Option::None),
                            |value| {
                                quote!(::core::option::Option::Some(
                                    #krate::__private::serde_json::json!(#value)
                                ))
                            },
                        )
                    };
                    let (min, max) = (bound(min), bound(max));
                    quote!(::core::option::Option::Some({
                        fn bounds() -> (
                            ::core::option::Option<#krate::__private::serde_json::Value>,
                            ::core::option::Option<#krate::__private::serde_json::Value>,
                        ) {
                            (#min, #max)
                        }
                        bounds as fn() -> _
                    }))
                } else {
                    quote!(::core::option::Option::None)
                };
                properties.push(quote! {
                    #krate::schema::Field {
                        name: #name,
                        schema: <#ty as #krate::JsonSchema>::json_schema,
                        required: !#defaulted && !<#ty as #krate::JsonSchema>::OPTIONAL,
                        doc: #doc,
                        range: #range,
                    }
                });
            }
            Some(quote!(#krate::schema::Content::Object(&[#(#properties),*])))
        }
        Fields::Unnamed(unnamed) => {
            let kept: Vec<_> = unnamed
                .unnamed
                .iter()
                .map(|field| Ok((field, item_options(&field.attrs, false)?)))
                .collect::<syn::Result<Vec<_>>>()?
                .into_iter()
                .filter(|(_, options)| !options.skip)
                .collect();
            match kept.as_slice() {
                [(field, _)] if unnamed.unnamed.len() == 1 => {
                    let ty = &field.ty;
                    Some(quote!(#krate::schema::Content::Newtype(
                        <#ty as #krate::JsonSchema>::json_schema
                    )))
                }
                many => {
                    let len = many.len();
                    Some(quote!(#krate::schema::Content::Tuple(#len)))
                }
            }
        }
        Fields::Unit => None,
    })
}

/// One alternative per variant, tagged as serde tags it (a `schema::Body`).
/// An enum of plain unit variants is a string enumeration.
fn enum_schema(
    input: &DeriveInput,
    data: &syn::DataEnum,
    options: &ContainerOptions,
) -> syn::Result<Tokens> {
    let krate = krate();
    let tagging = match (&options.tag, &options.content) {
        (None, None) => Tagging::External,
        (Some(tag), None) => Tagging::Internal(tag.clone()),
        (Some(tag), Some(content)) => Tagging::Adjacent(tag.clone(), content.clone()),
        (None, Some(_)) => {
            return Err(syn::Error::new(
                input.span(),
                "serde `content` needs a `tag`",
            ));
        }
    };
    let mut variants = Vec::new();
    let mut plain = matches!(tagging, Tagging::External);
    for variant in &data.variants {
        let variant_options = item_options(&variant.attrs, true)?;
        if variant_options.skip {
            continue;
        }
        let name = json_name(
            &variant.ident,
            &variant_options,
            options.rename_all.as_ref(),
        )?;
        let variant_doc = plain_doc(&variant.attrs);
        plain &= matches!(variant.fields, Fields::Unit) && variant_doc.is_empty();
        let tuple = matches!(&variant.fields, Fields::Unnamed(fields) if fields.unnamed.len() != 1);
        if tuple && matches!(tagging, Tagging::Internal(_)) {
            return Err(syn::Error::new(
                variant.span(),
                "serde cannot tag a tuple variant internally",
            ));
        }
        let content = fields_schema(&variant.fields, variant_options.rename_all.as_ref(), false)?
            .map_or_else(
                || quote!(::core::option::Option::None),
                |content| quote!(::core::option::Option::Some(#content)),
            );
        variants.push(quote!(#krate::schema::Variant {
            name: #name,
            doc: #variant_doc,
            content: #content,
        }));
    }
    let tagging = match tagging {
        Tagging::External => quote!(#krate::schema::Tagging::External),
        Tagging::Internal(tag) => quote!(#krate::schema::Tagging::Internal(#tag)),
        Tagging::Adjacent(tag, key) => quote!(#krate::schema::Tagging::Adjacent(#tag, #key)),
    };
    Ok(quote!(#krate::schema::Body::Enum {
        tagging: #tagging,
        plain: #plain,
        variants: &[#(#variants),*],
    }))
}

pub fn derive(input: &DeriveInput) -> syn::Result<Tokens> {
    let krate = krate();
    let options = container_options(&input.attrs)?;
    let doc = plain_doc(&input.attrs);
    let body = match &input.data {
        Data::Struct(data) => {
            if options.transparent {
                let field = data
                    .fields
                    .iter()
                    .find(|field| !item_options(&field.attrs, false).is_ok_and(|o| o.skip))
                    .ok_or_else(|| {
                        syn::Error::new(input.span(), "a transparent type needs a field")
                    })?;
                let ty = &field.ty;
                quote!(#krate::schema::Body::Transparent(
                    <#ty as #krate::JsonSchema>::json_schema
                ))
            } else {
                fields_schema(&data.fields, options.rename_all.as_ref(), options.default)?
                    .map_or_else(
                        || quote!(#krate::schema::Body::Unit),
                        |content| quote!(#krate::schema::Body::Struct(#content)),
                    )
            }
        }
        Data::Enum(data) => enum_schema(input, data, &options)?,
        Data::Union(_) => {
            return Err(syn::Error::new(
                input.span(),
                "JsonSchema describes structs and enums",
            ));
        }
    };

    let ident = &input.ident;
    let mut generics = input.generics.clone();
    for param in generics.type_params_mut() {
        param.bounds.push(syn::parse_quote!(#krate::JsonSchema));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #krate::JsonSchema for #ident #ty_generics #where_clause {
            fn json_schema() -> #krate::__private::serde_json::Value {
                #krate::schema::build(const {
                    &#krate::schema::TypeSchema { doc: #doc, body: #body }
                })
            }
        }
    })
}
