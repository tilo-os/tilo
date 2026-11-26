//! `tilo_resource::bindings!`: a provider's actions, as a caller in Rust calls them, from the
//! compiled surface the provider publishes.
//!
//! The provider declares its actions once, in its code (`#[derive(Effect)]`), and its test
//! writes the surface that declaration publishes. A caller names that file and the provider's
//! id, and gets, at build time, what it needs to call each action and read its answer:
//!
//! - `PROVIDER`, and per resource kind a module (`environment` for `environment`) with its
//!   `RESOURCE` and, per action, the action's id (`BRANCH_MACHINE`) and its target
//!   (`branch_machine()`);
//! - per action that takes parameters, its arguments as a struct (`BranchMachine`), its fields
//!   the parameters, an optional one an `Option`; one that takes a secret does not print;
//! - per resource kind whose surface reads as fields, that reading as a record named after the
//!   kind (`Models` for `models`);
//! - the records and enums the parameters and answers name, declared once at the top. A list,
//!   a map, a flag or an option a record leaves out reads as empty, as its provider writes it.
//!
//! A caller uses the few actions it calls, so nothing generated is dead code when unused.
//!
//! Nothing it writes is committed: the surface is the one declaration, and a changed surface
//! changes the bindings with the next build.

use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use serde_json::Value;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Token};

/// `provider = "…", surface = "…"`.
pub struct Input {
    provider: LitStr,
    surface: LitStr,
}

impl Parse for Input {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut provider = None;
        let mut surface = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: LitStr = input.parse()?;
            match key.to_string().as_str() {
                "provider" => provider = Some(value),
                "surface" => surface = Some(value),
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "expected `provider` or `surface`",
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        let missing = |what: &str| syn::Error::new(Span::call_site(), format!("missing `{what}`"));
        Ok(Self {
            provider: provider.ok_or_else(|| missing("provider"))?,
            surface: surface.ok_or_else(|| missing("surface"))?,
        })
    }
}

/// The bindings `input` names.
pub fn expand(input: &Input) -> syn::Result<Tokens> {
    let error = |message: String| syn::Error::new(input.surface.span(), message);
    let dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| error("no CARGO_MANIFEST_DIR to find the surface from".into()))?;
    let path = dir.join(input.surface.value());
    let json = std::fs::read_to_string(&path)
        .map_err(|why| error(format!("{}: {why}", path.display())))?;
    let surface: Value =
        serde_json::from_str(&json).map_err(|why| error(format!("{}: {why}", path.display())))?;
    if tilo_canonical_json::to_string(&surface) != json {
        return Err(error(format!(
            "{}: not the canonical compiled form of a surface; its provider's test writes it",
            path.display()
        )));
    }
    let tracked = path.display().to_string();
    let provider = &input.provider;
    let mut named = Vec::new();
    let mut declared = std::collections::BTreeSet::new();
    let mut modules = Vec::new();
    for resource in list(&surface, "resources") {
        for ty in list(resource, "types") {
            let name = text(ty, "name");
            if declared.insert(name.to_owned()) {
                named.push(named_type(ty).map_err(error)?);
            }
        }
        if let Some(reading) = reading(resource).map_err(error)? {
            named.push(reading);
        }
        modules.push(resource_module(resource).map_err(error)?);
    }
    let unused = unused();
    let items = named
        .into_iter()
        .chain(modules)
        .map(|item| quote!(#unused #item));
    Ok(quote! {
        const _: &[u8] = ::core::include_bytes!(#tracked);
        /// The provider these actions are asked of.
        #unused
        pub const PROVIDER: &str = #provider;
        #(#items)*
    })
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// `kebab-case`, `snake_case` or `dotted.name` as `CamelCase`.
fn camel(name: &str) -> String {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

/// A wire name as a Rust identifier, raw where it is a keyword: `signIns` is `sign_ins`.
fn snake(name: &str) -> Ident {
    let mut snake = String::with_capacity(name.len().saturating_mul(2));
    let mut lower_before = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() && lower_before {
            snake.push('_');
        }
        lower_before = c.is_ascii_lowercase() || c.is_ascii_digit();
        snake.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '_'
        });
    }
    syn::parse_str::<Ident>(&snake).unwrap_or_else(|_| format_ident!("r#{}", snake))
}

/// A field type of the surface as the Rust type its JSON reads into.
fn rust_type(ty: &Value) -> Result<Tokens, String> {
    // A surface leaves out a type the schema says all of: any value.
    if ty.is_null() {
        return Ok(quote!(::serde_json::Value));
    }
    if let Some(name) = ty.as_str() {
        return Ok(match name {
            "text" | "secret" | "rich_text" | "identifier" | "actor" | "date" | "time" => {
                quote!(::std::string::String)
            }
            "int" | "duration" => quote!(i64),
            "float" => quote!(f64),
            "bool" => quote!(bool),
            "any" | "json" | "reactions" => quote!(::serde_json::Value),
            other => return Err(format!("a field type the bindings do not know: {other}")),
        });
    }
    let Some((kind, inner)) = ty.as_object().and_then(|object| object.iter().next()) else {
        return Err(format!("a field type the bindings do not know: {ty}"));
    };
    Ok(match kind.as_str() {
        "resource" => quote!(::std::string::String),
        "named" => {
            let name = format_ident!("{}", inner.as_str().unwrap_or_default());
            quote!(#name)
        }
        "option" => {
            let inner = rust_type(inner)?;
            quote!(::core::option::Option<#inner>)
        }
        "list" | "set" | "collection" => {
            let inner = rust_type(inner)?;
            quote!(::std::vec::Vec<#inner>)
        }
        "map" => {
            let inner = rust_type(inner)?;
            quote!(::std::collections::BTreeMap<::std::string::String, #inner>)
        }
        "union" | "outbox" => quote!(::serde_json::Value),
        other => return Err(format!("a field type the bindings do not know: {other}")),
    })
}

/// One field: its wire name, its type, and whether it may be left out. In an enum's case it
/// is written without `pub`, which a variant's fields may not carry.
fn field(
    name: &str,
    ty: &Value,
    optional: bool,
    doc: &str,
    public: bool,
) -> Result<Tokens, String> {
    let ident = snake(name);
    let mut rust = rust_type(ty)?;
    let is_option = ty.get("option").is_some();
    let rename = (ident.to_string().trim_start_matches("r#") != name)
        .then(|| quote!(#[serde(rename = #name)]));
    let attrs = if is_option {
        quote!(#[serde(default, skip_serializing_if = "::core::option::Option::is_none")])
    } else if optional {
        rust = quote!(::core::option::Option<#rust>);
        quote!(#[serde(default, skip_serializing_if = "::core::option::Option::is_none")])
    } else if empty_when_left_out(ty) {
        quote!(#[serde(default)])
    } else {
        Tokens::new()
    };
    let doc = if doc.is_empty() { name } else { doc };
    let public = public.then(|| quote!(pub));
    Ok(quote! {
        #[doc = #doc]
        #rename
        #attrs
        #public #ident: #rust,
    })
}

/// Whether a value of the field type `ty` that its record leaves out reads as empty: a list, a
/// map or a flag, which a provider leaves out when it is empty or off.
fn empty_when_left_out(ty: &Value) -> bool {
    ty.as_str() == Some("bool")
        || ["list", "set", "collection", "map"]
            .iter()
            .any(|kind| ty.get(kind).is_some())
}

const DERIVES: &str = "Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize";

fn derives() -> Tokens {
    let derives: Tokens = DERIVES.parse().unwrap_or_default();
    quote!(#[derive(#derives)])
}

/// What lets a caller leave unused what it does not call.
fn unused() -> Tokens {
    quote!(#[allow(
        dead_code,
        reason = "a caller calls the few actions it needs of all those a surface declares"
    )])
}

/// The record a resource kind reads as, named after the kind, when its surface lists fields.
fn reading(resource: &Value) -> Result<Option<Tokens>, String> {
    let fields = list(resource, "fields");
    if fields.is_empty() {
        return Ok(None);
    }
    let kind = text(resource, "kind");
    let ident = format_ident!("{}", camel(kind));
    let fields = fields
        .iter()
        .map(|one| {
            field(
                text(one, "path"),
                &one["type"],
                false,
                text(one, "title"),
                true,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let doc = format!("What `{kind}` reads as.");
    let derives = derives();
    Ok(Some(quote! {
        #[doc = #doc]
        #derives
        pub struct #ident { #(#fields)* }
    }))
}

/// A record or enum the surface declares by name.
fn named_type(ty: &Value) -> Result<Tokens, String> {
    let name = text(ty, "name");
    let ident = format_ident!("{}", name);
    let doc = match text(ty, "doc") {
        "" => name,
        doc => doc,
    };
    let derives = derives();
    let cases = list(ty, "cases");
    if cases.is_empty() {
        let fields = list(ty, "fields")
            .iter()
            .map(|one| {
                field(
                    text(one, "name"),
                    &one["type"],
                    false,
                    text(one, "title"),
                    true,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(quote! {
            #[doc = #doc]
            #derives
            pub struct #ident { #(#fields)* }
        });
    }
    let variants = cases
        .iter()
        .map(|case| {
            let wire = text(case, "name");
            let variant = format_ident!("{}", camel(wire));
            let label = match text(case, "label") {
                "" => wire,
                label => label,
            };
            let fields = list(case, "fields")
                .iter()
                .map(|one| {
                    field(
                        text(one, "name"),
                        &one["type"],
                        false,
                        text(one, "title"),
                        false,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let fields = (!fields.is_empty()).then(|| quote!({ #(#fields)* }));
            Ok(quote! {
                #[doc = #label]
                #[serde(rename = #wire)]
                #variant #fields,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let tag = ty
        .get("tag")
        .and_then(Value::as_str)
        .map(|tag| quote!(#[serde(tag = #tag)]));
    Ok(quote! {
        #[doc = #doc]
        #derives
        #tag
        pub enum #ident { #(#variants)* }
    })
}

/// A resource kind's module: its name, and each action's id, target and arguments.
fn resource_module(resource: &Value) -> Result<Tokens, String> {
    let kind = text(resource, "kind");
    let module = snake(kind);
    let mut items = Vec::new();
    for verb in list(resource, "verbs") {
        let id = text(verb, "id");
        let constant = format_ident!("{}", id.to_uppercase().replace(['-', '.'], "_"));
        let target = snake(id);
        let label = match text(verb, "label") {
            "" => id,
            label => label,
        };
        items.push(quote! {
            #[doc = #label]
            pub const #constant: &str = #id;
            #[doc = #label]
            #[must_use]
            pub fn #target() -> ::tilo_actions::remote::RemoteActionTarget {
                ::tilo_actions::remote::RemoteActionTarget {
                    provider: super::PROVIDER.to_owned(),
                    address: ::tilo_actions::api::ActionAddress::new(RESOURCE, #constant),
                }
            }
        });
        let params = list(verb, "params");
        if params.is_empty() {
            continue;
        }
        let name = format_ident!("{}", camel(id));
        let fields = params
            .iter()
            .map(|param| {
                let required = param.get("required").and_then(Value::as_bool) == Some(true);
                field(
                    text(param, "name"),
                    &param["type"],
                    !required,
                    text(&param["schema"], "description"),
                    true,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let doc = format!("What `{id}` takes.");
        // A secret it takes is handed over once: arguments holding one do not print.
        let derives = if params.iter().any(|param| param["type"] == "secret") {
            quote!(#[derive(Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize)])
        } else {
            derives()
        };
        items.push(quote! {
            #[doc = #doc]
            #derives
            pub struct #name { #(#fields)* }
        });
    }
    let doc = format!("The actions of `{kind}`.");
    Ok(quote! {
        #[doc = #doc]
        pub mod #module {
            #[allow(unused_imports)]
            use super::*;
            /// The resource these actions are of.
            pub const RESOURCE: &str = #kind;
            #(#items)*
        }
    })
}
