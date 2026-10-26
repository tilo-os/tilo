//! Procedural macros for `tilo-resource`. See that crate for the API; this
//! crate only generates calls into it.

use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, Fields, GenericArgument, ItemImpl, Lit, Meta,
    MetaNameValue, PathArguments, Type, parse_macro_input,
};

const CRATE: &str = "tilo_resource";

/// Generated code names the runtime crate by a relative path, so it resolves
/// either as a direct dependency or through `tilo_app::prelude::*`, which
/// re-exports it under the same name.
fn krate() -> Tokens {
    let name = format_ident!("{}", CRATE);
    quote!(#name)
}

/// Derive `Model` (and `Fields` for structs with named fields).
///
/// * a struct with named fields is a map, one key per field;
/// * a newtype struct is its inner value;
/// * an enum of unit variants is a string naming the variant; the variant
///   marked `#[default]` or `#[model(default)]` is the value of an absent slot
///   (`#[model(default)]` also marks a variant that carries fields);
/// * an enum whose variants carry fields is a map: the variant's name under
///   `$variant` and each field under its own key (a tuple variant's as `0`,
///   `1`, …), so edits of one variant merge field by field and a change of
///   variant is last writer wins. A field path names its variant first
///   (`shape.rect.width`); fields the new variant does not carry stay as
///   unused history for a later change back.
///
/// Field attributes: `#[model(rename = "key")]`, `#[model(skip)]` (not
/// stored; the containing type's `Default` on read), `#[model(atom)]` on a
/// `String` (written whole, last writer wins, as `Atom` is),
/// `#[model(whole)]` (read every part of a value that loads on demand,
/// rich text's paragraphs), `#[model(keyed)]` on a `Vec<T>` whose items carry
/// their own stable id (stored under those ids, so edits of different items
/// merge; the in-memory value stays a `Vec`), `#[model(resource)]` on a field whose type is a
/// resource with its own operations (rich text: published beneath the
/// record's resource, an item's or the record's own, at `<record>/<key>`,
/// its actions applied as field edits; see `tilo_resource::child`), `#[model(remove = "quiet")]` on a collection whose items are cheap to
/// remove (removing one by field edit asks no confirmation),
/// `#[model(default = expr)]` (what the field reads as when the document
/// holds none: a required field with a sensible derivation, so an item
/// missing it still reads; the next whole write stores it),
/// `#[model(trust = "sensitive")]` (every edit of it asks for confirmation:
/// a change can expose the person), `#[model(trust = "weakening")]`
/// (it also crosses a security boundary: any change can weaken the person's
/// security, so an agent or script acting for them needs their own grant)
/// and `#[model(weaker = ..)]` (only a change making it weaker than it is
/// now does: `weaker = "higher"` or `"lower"` for a number, with
/// `weakest = N` for the value weaker than any, such as a lock delay's `0`
/// for never; `weaker = true` or `false` for a switch; a list of values
/// from the strongest to the weakest for a choice).
///
/// A struct's `Model::SHAPE` lists its stored fields, so layout-aware services
/// (History restore) reconcile it field by field.
/// The app's committed `surface.toml` (the file its package ships), as
/// `Option<&'static [u8]>`: `Some` with the file's bytes when the crate
/// that expands it has one at its root, else `None`.
#[proc_macro]
pub fn committed_surface(_input: TokenStream) -> TokenStream {
    let path = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .map(|dir| dir.join("surface.toml"))
        .filter(|path| path.is_file());
    if let Some(path) = path.and_then(|path| path.to_str().map(str::to_owned)) {
        quote::quote! {
            ::core::option::Option::Some(::core::include_bytes!(#path) as &'static [u8])
        }
    } else {
        quote::quote! { ::core::option::Option::<&'static [u8]>::None }
    }
    .into()
}

#[proc_macro_derive(Model, attributes(model))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derive `JsonSchema`: the JSON Schema of the type's serde JSON form.
///
/// Reads the serde attributes that change the form — `rename`,
/// `rename_all`, `tag`, `content`, `transparent`, `default`, `skip` — and
/// refuses those it cannot describe (`flatten`, `untagged`, `with`), so the
/// schema never differs from what serde reads. Doc comments become
/// descriptions. Every field's type must implement `JsonSchema` too.
///
/// `#[schema(range(min = 1, max = 100))]` on a numeric field narrows its
/// bounds (either side may be left out): the range is published in the
/// schema and binding refuses a value outside it, so a `u8` that means
/// 1 to 100 does not read as `0..255`.
#[proc_macro_derive(JsonSchema, attributes(serde, schema))]
pub fn derive_json_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match schema::derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

mod data_enum;
mod schema;

/// Derive `Effect`: an enum's variants become the app's published effects —
/// work with no document delta, invoked by buttons, the palette, agents and
/// scripts alike. See `tilo_resource::effect`.
///
/// `#[effect(operation = "app.effects")]` names the vocabulary's operation
/// type. Each variant is unit or has named fields (the effect's typed
/// parameters, `#[param("…")]` describing one); its id is the variant name in
/// kebab case and its label the first doc line. Variant options:
/// `#[effect(label = "...", view, row, job, cancellable, returns,
/// destructive, privileged, idempotent, own_device, attended, audience = "agents", untrusted, opens_path, member, risk = "path", kind = "read",
/// host_render, tag = some::TAG)]`. `host_render` marks an effect a host
/// answers in a render service of its own rather than the record's app
/// worker; `tag` (repeatable) adds a contract tag, a `&'static str` constant;
/// `outside_with = "path"` makes a `kind = "read"` effect reach outside only
/// when that parameter is given (an export writing the file it names).
/// Who may run an effect is decided by its kind, like any action. An
/// `own_device` effect's whole scope is the invoking person's own device,
/// touching nothing shared and no one else (their clipboard, their own
/// appearance): a room's grants have nothing to consent to. An `attended`
/// effect needs its person attending (a chooser, a sign-in, a secret shown):
/// whoever acts for them is always asked first, while the person's own click
/// is their decision and, like an `own_device` effect, needs no room's
/// consent and runs only on their own device. Generates `impl Effect` and one `ActionRef`
/// constant per variant (`Backups::RESTORE`) for binding controls.
#[proc_macro_derive(Effect, attributes(effect, param))]
pub fn derive_effect(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match effect::derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

mod effect;

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent field attribute"
)]
struct FieldOptions {
    rename: Option<String>,
    skip: bool,
    label: bool,
    readonly: bool,
    unit: bool,
    atom: bool,
    keyed: bool,
    quiet_remove: bool,
    whole: bool,
    resource: bool,
    /// `default = expr`: what an absent field reads as.
    default: Option<Expr>,
    /// `default` without a value: the variant an absent data enum reads as.
    default_variant: bool,
    /// `trust = "sensitive" | "weakening"`: the `Trust` variant it names.
    trust: Option<syn::LitStr>,
    /// `weaker = "higher" | "lower" | bool | ["strongest", .., "weakest"]`:
    /// which way a change weakens security.
    weaker: Option<Expr>,
    /// `weakest = N`: the number weaker than any (a delay's `0`, never).
    weakest: Option<syn::LitInt>,
}

impl FieldOptions {
    /// The field's `Trust`, as an expression.
    fn trust(&self, krate: &Tokens) -> syn::Result<Tokens> {
        let named = self.trust.as_ref().map(syn::LitStr::value);
        let numeric = matches!(
            &self.weaker,
            Some(Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            }))
        );
        if let Some(weakest) = self.weakest.as_ref().filter(|_| !numeric) {
            return Err(syn::Error::new(
                weakest.span(),
                "`weakest` goes with `weaker = \"higher\"` or `weaker = \"lower\"`",
            ));
        }
        let Some(weaker) = &self.weaker else {
            return Ok(match named.as_deref() {
                None => quote!(#krate::Trust::Ordinary),
                Some("sensitive") => quote!(#krate::Trust::Sensitive),
                Some("weakening") => quote!(#krate::Trust::Weakening),
                Some(_) => {
                    return Err(syn::Error::new(
                        self.trust.span(),
                        "expected `trust = \"sensitive\"` or `trust = \"weakening\"`",
                    ));
                }
            });
        };
        if named.as_deref().is_some_and(|named| named != "weakening") {
            return Err(syn::Error::new(
                self.trust.span(),
                "`weaker` makes the field weaken one way: drop `trust` or make it \"weakening\"",
            ));
        }
        let unexpected = || {
            syn::Error::new(
                weaker.span(),
                "expected `weaker = \"higher\"`, `\"lower\"`, `true`, `false` or a list of \
                 values from the strongest to the weakest",
            )
        };
        let weakest = if let Some(weakest) = &self.weakest {
            let weakest: i64 = weakest.base10_parse()?;
            quote!(::core::option::Option::Some(#weakest))
        } else {
            quote!(::core::option::Option::None)
        };
        let string = |expr: &Expr| match expr {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(text),
                ..
            }) => Some(text.value()),
            _ => None,
        };
        let weaker = match weaker {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(rule),
                ..
            }) if rule.value() == "higher" => {
                quote!(#krate::Weaker::Higher { weakest: #weakest })
            }
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(rule),
                ..
            }) if rule.value() == "lower" => quote!(#krate::Weaker::Lower { weakest: #weakest }),
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Bool(weak),
                ..
            }) => {
                let order = if weak.value {
                    ["false", "true"]
                } else {
                    ["true", "false"]
                };
                quote!(#krate::Weaker::Order(&[#(#order),*]))
            }
            Expr::Array(values) => {
                // Each value as the JSON text of the string it serializes to.
                let order = values
                    .elems
                    .iter()
                    .map(|value| {
                        string(value)
                            .filter(|text| {
                                !text
                                    .chars()
                                    .any(|c| c == '"' || c == '\\' || c.is_control())
                            })
                            .map(|text| format!("\"{text}\""))
                            .ok_or_else(unexpected)
                    })
                    .collect::<syn::Result<Vec<_>>>()?;
                quote!(#krate::Weaker::Order(&[#(#order),*]))
            }
            _ => return Err(unexpected()),
        };
        Ok(quote!(#krate::Trust::Weakens(#weaker)))
    }
}

fn field_options(attrs: &[Attribute]) -> syn::Result<FieldOptions> {
    let mut options = FieldOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("model")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                options.skip = true;
            } else if meta.path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                options.rename = Some(value.value());
            } else if meta.path.is_ident("label") {
                options.label = true;
            } else if meta.path.is_ident("readonly") {
                options.readonly = true;
            } else if meta.path.is_ident("unit") {
                options.unit = true;
            } else if meta.path.is_ident("atom") {
                options.atom = true;
            } else if meta.path.is_ident("keyed") {
                options.keyed = true;
            } else if meta.path.is_ident("whole") {
                options.whole = true;
            } else if meta.path.is_ident("resource") {
                options.resource = true;
            } else if meta.path.is_ident("default") {
                if meta.input.peek(syn::Token![=]) {
                    options.default = Some(meta.value()?.parse()?);
                } else {
                    options.default_variant = true;
                }
            } else if meta.path.is_ident("trust") {
                options.trust = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("weaker") {
                options.weaker = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("weakest") {
                options.weakest = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("remove") {
                let how: syn::LitStr = meta.value()?.parse()?;
                if how.value() != "quiet" {
                    return Err(meta.error("expected `remove = \"quiet\"`"));
                }
                options.quiet_remove = true;
            } else {
                return Err(meta.error(
                    "expected `skip`, `rename = \"...\"`, `label`, `readonly`, `unit`, `atom`, \
                     `keyed`, `whole`, `resource`, `default = ..`, `trust = \"..\"`, \
                     `weaker = ..`, `weakest = N` or `remove = \"quiet\"`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(options)
}

/// What `#[model(..)]` on a record type says.
#[derive(Default)]
struct ContainerOptions {
    /// `check = path`: its invariant.
    check: Option<syn::Path>,
    /// `label = "first last"`: the fields whose texts, joined by spaces,
    /// name a value.
    label: Vec<String>,
    /// `check_reads = "a b"`: the fields the invariant reads; an edit of
    /// none of them (nor of an item of `check_items`) cannot break it.
    check_reads: Option<Vec<String>>,
    /// `check_items = "c"`: the collections whose items the invariant
    /// counts (not reads): adding or removing one can break it, an edit
    /// inside one cannot.
    check_items: Vec<String>,
}

/// `audience = "people" | "agents" | "everyone"` on an action or effect:
/// the `tilo_actions::Audience` variant it names.
fn audience_option(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<syn::Ident> {
    let value = meta.value()?.parse::<syn::LitStr>()?;
    let variant = match value.value().as_str() {
        "everyone" => "Everyone",
        "people" => "People",
        "agents" => "Agents",
        _ => {
            return Err(syn::Error::new(
                value.span(),
                "expected `audience = \"people\"`, `\"agents\"` or `\"everyone\"`",
            ));
        }
    };
    Ok(syn::Ident::new(variant, value.span()))
}

/// Fields named by a space-separated `"a b"` list.
fn field_list(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<Vec<String>> {
    let fields = meta.value()?.parse::<syn::LitStr>()?;
    Ok(fields
        .value()
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// The `T` of a field whose type is `Vec<T>`.
fn vec_item(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| match argument {
        syn::GenericArgument::Type(item) => Some(item),
        _ => None,
    })
}

/// `#[model(check = path, label = "first last")]` on the type.
fn container_options(attrs: &[Attribute]) -> syn::Result<ContainerOptions> {
    let mut options = ContainerOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("model")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("check") {
                options.check = Some(meta.value()?.parse::<syn::Path>()?);
                Ok(())
            } else if meta.path.is_ident("check_reads") {
                options.check_reads = Some(field_list(&meta)?);
                Ok(())
            } else if meta.path.is_ident("check_items") {
                options.check_items = field_list(&meta)?;
                Ok(())
            } else if meta.path.is_ident("label") {
                options.label = field_list(&meta)?;
                if options.label.is_empty() {
                    return Err(meta.error("`label = \"..\"` names the fields that make the label"));
                }
                Ok(())
            } else {
                Err(meta.error(
                    "expected `check = path`, `check_reads = \"field ..\"`, \
                     `check_items = \"field ..\"` or `label = \"field field ..\"` on the type",
                ))
            }
        })?;
    }
    if options.check.is_none() && (options.check_reads.is_some() || !options.check_items.is_empty())
    {
        return Err(syn::Error::new(
            Span::call_site(),
            "`check_reads` and `check_items` say what a `check = path` reads",
        ));
    }
    // A field named twice is not the label twice: the second mention would
    // silently name no field, and the label would read as if it had.
    let mut named: Vec<&str> = Vec::new();
    for part in &options.label {
        if named.contains(&part.as_str()) {
            return Err(syn::Error::new(
                Span::call_site(),
                format!("`label = \"..\"` names `{part}` twice"),
            ));
        }
        named.push(part);
    }
    Ok(options)
}

#[allow(
    clippy::too_many_lines,
    reason = "one expansion per data shape, each read top to bottom"
)]
fn derive(input: &DeriveInput) -> syn::Result<Tokens> {
    let krate = krate();
    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => derive_named(input, fields),
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                let ty = fields
                    .unnamed
                    .first()
                    .map(|field| &field.ty)
                    .ok_or_else(|| syn::Error::new(fields.span(), "expected one field"))?;
                Ok(quote! {
                    impl #impl_generics #krate::Model for #name #type_generics #where_clause {
                        const SHAPE: #krate::Shape = <#ty as #krate::Model>::SHAPE;
                        const CHECKS: bool = <#ty as #krate::Model>::CHECKS;
                        const SCALAR: bool = <#ty as #krate::Model>::SCALAR;
                        const NULLABLE: bool = <#ty as #krate::Model>::NULLABLE;
                        const OPS: bool = <#ty as #krate::Model>::OPS;
                        type Paths<__R> = #krate::Path<__R, Self>;

                        fn paths_at<__R>(path: #krate::Path<__R, Self>) -> #krate::Path<__R, Self> {
                            path
                        }
                        fn to_json(&self) -> #krate::__private::serde_json::Value {
                            #krate::Model::to_json(&self.0)
                        }
                        fn from_json(
                            value: &#krate::__private::serde_json::Value,
                        ) -> ::core::result::Result<Self, #krate::ModelError> {
                            <#ty as #krate::Model>::from_json(value).map(Self)
                        }
                        fn edit(
                            doc: &mut #krate::Doc,
                            slot: #krate::Slot<'_>,
                            path: &[#krate::Segment],
                            op: &#krate::EditOp,
                            cx: &mut #krate::ActionCx,
                        ) -> ::core::result::Result<(), #krate::ModelError> {
                            <#ty as #krate::Model>::edit(doc, slot, path, op, cx)
                        }
                        fn check(&self) -> ::core::result::Result<(), #krate::Refusal> {
                            #krate::Model::check(&self.0)
                        }
                        fn label(&self) -> ::core::option::Option<::std::string::String> {
                            #krate::Model::label(&self.0)
                        }

                        fn load(
                            doc: &#krate::Doc,
                            value: &#krate::__private::Value<'_>,
                            id: &#krate::__private::ObjId,
                        ) -> ::core::result::Result<Self, #krate::ModelError> {
                            <#ty as #krate::Model>::load(doc, value, id).map(Self)
                        }
                        fn value_schema() -> #krate::__private::serde_json::Value {
                            <#ty as #krate::Model>::value_schema()
                        }
                        fn missing() -> ::core::result::Result<Self, #krate::ModelError> {
                            <#ty as #krate::Model>::missing().map(Self)
                        }
                        fn store(
                            &self,
                            doc: &mut #krate::Doc,
                            slot: #krate::Slot<'_>,
                        ) -> ::core::result::Result<(), #krate::ModelError> {
                            #krate::Model::store(&self.0, doc, slot)
                        }
                        fn stores_as(&self, before: &Self) -> bool {
                            #krate::Model::stores_as(&self.0, &before.0)
                        }
                        fn store_changed(
                            &self,
                            before: &Self,
                            doc: &mut #krate::Doc,
                            slot: #krate::Slot<'_>,
                            dirty: &#krate::Delta,
                        ) -> ::core::result::Result<(), #krate::ModelError> {
                            #krate::Model::store_changed(&self.0, &before.0, doc, slot, dirty)
                        }
                        fn refresh(
                            &mut self,
                            doc: &#krate::Doc,
                            value: &#krate::__private::Value<'_>,
                            id: &#krate::__private::ObjId,
                            delta: &#krate::Delta,
                        ) -> ::core::result::Result<(), #krate::ModelError> {
                            #krate::Model::refresh(&mut self.0, doc, value, id, delta)
                        }
                        fn collect_text(&self, out: &mut ::std::vec::Vec<::std::string::String>) {
                            #krate::Model::collect_text(&self.0, out);
                        }
                    }
                })
            }
            _ => Err(syn::Error::new(
                input.span(),
                "Model derives for structs with named fields or one unnamed field",
            )),
        },
        Data::Enum(data)
            if data
                .variants
                .iter()
                .any(|variant| !matches!(variant.fields, Fields::Unit)) =>
        {
            data_enum::derive(input, data)
        }
        Data::Enum(data) => {
            let mut names = Vec::new();
            let mut idents = Vec::new();
            let mut default = None;
            for variant in &data.variants {
                let options = field_options(&variant.attrs)?;
                let key = options
                    .rename
                    .unwrap_or_else(|| snake_case(&variant.ident.to_string()));
                if options.default_variant
                    || variant
                        .attrs
                        .iter()
                        .any(|attr| attr.path().is_ident("default"))
                {
                    default = Some(variant.ident.clone());
                }
                names.push(key);
                idents.push(variant.ident.clone());
            }
            let table = quote!(#krate::table);
            let positions: Vec<proc_macro2::Literal> = (0..idents.len())
                .map(proc_macro2::Literal::usize_unsuffixed)
                .collect();
            let default_index = default.map_or_else(
                || quote!(::core::option::Option::None),
                |ident| {
                    let position = idents.iter().position(|candidate| *candidate == ident);
                    let position = proc_macro2::Literal::usize_unsuffixed(position.unwrap_or(0));
                    quote!(::core::option::Option::Some(#position))
                },
            );
            Ok(quote! {
                impl #impl_generics #name #type_generics #where_clause {
                    /// The enum as data: its variants' stored names.
                    #[doc(hidden)]
                    pub const __UNIT: #table::UnitDesc = #table::UnitDesc {
                        names: &[#(#names),*],
                        default: #default_index,
                        index: Self::__index,
                        make: Self::__make,
                        set: Self::__set,
                        staged: (#table::empty_of::<Self>, #table::unwrap_of::<Self>),
                    };

                    fn __index(value: &dyn ::core::any::Any) -> ::core::option::Option<usize> {
                        match value.downcast_ref::<Self>()? {
                            #(Self::#idents => ::core::option::Option::Some(#positions),)*
                        }
                    }

                    fn __variant(index: usize) -> ::core::option::Option<Self> {
                        match index {
                            #(#positions => ::core::option::Option::Some(Self::#idents),)*
                            _ => ::core::option::Option::None,
                        }
                    }

                    fn __make(
                        index: usize,
                        out: &mut dyn ::core::any::Any,
                    ) -> ::core::result::Result<(), #krate::ModelError> {
                        #table::put_variant(Self::__variant(index), out)
                    }

                    fn __set(
                        index: usize,
                        target: &mut dyn ::core::any::Any,
                    ) -> ::core::result::Result<(), #krate::ModelError> {
                        #table::set_variant(Self::__variant(index), target)
                    }
                }

                impl #impl_generics #krate::Settable for #name #type_generics #where_clause {}

                impl #impl_generics #krate::Model for #name #type_generics #where_clause {
                    const TABLE: &'static #table::ModelTable = &#table::ModelTable::of_unit(&Self::__UNIT);
                    type Paths<__R> = #krate::Path<__R, Self>;

                    fn paths_at<__R>(path: #krate::Path<__R, Self>) -> #krate::Path<__R, Self> {
                        path
                    }
                    #krate::__table_functions!();
                    fn value_schema() -> #krate::__private::serde_json::Value {
                        #table::unit_schema(Self::__UNIT.names)
                    }
                    #[inline]
                    fn missing() -> ::core::result::Result<Self, #krate::ModelError> {
                        #table::Run(Self::TABLE).missing()
                    }
                }
            })
        }
        Data::Union(_) => Err(syn::Error::new(
            input.span(),
            "Model does not derive for unions",
        )),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear expansion: the field table, the reach into a record, and the trait impls"
)]
fn derive_named(input: &DeriveInput, fields: &syn::FieldsNamed) -> syn::Result<Tokens> {
    let krate = krate();
    let table = quote!(#krate::table);
    let name = &input.ident;
    let vis = &input.vis;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let ContainerOptions {
        check,
        label: label_parts,
        check_reads,
        check_items,
    } = container_options(&input.attrs)?;
    // Each stored field's key by its name, for what the check reads.
    let mut keys: Vec<(String, String)> = Vec::new();
    let own_check_const = check.is_some();
    let paths_name = format_ident!("{}Paths", name);
    let mut paths_generics = input.generics.clone();
    paths_generics.params.insert(0, syn::parse_quote!(__R));
    let (paths_impl_generics, paths_type_generics, _) = paths_generics.split_for_impl();
    let generic_args: Vec<Tokens> = input
        .generics
        .params
        .iter()
        .map(|param| match param {
            syn::GenericParam::Type(ty) => {
                let ident = &ty.ident;
                quote!(#ident)
            }
            syn::GenericParam::Lifetime(lifetime) => {
                let lifetime = &lifetime.lifetime;
                quote!(#lifetime)
            }
            syn::GenericParam::Const(constant) => {
                let ident = &constant.ident;
                quote!(#ident)
            }
        })
        .collect();

    // The record's stored fields, in declaration order: what the field
    // table lists, what `__field` reaches by position, and what `__build`
    // takes from its source one after the other.
    let mut descs = Vec::new();
    let mut builds = Vec::new();
    let mut reach = Vec::new();
    let mut reach_mut = Vec::new();
    let mut shapes = Vec::new();
    let mut checks_const = vec![if check.is_some() {
        quote!(true)
    } else {
        quote!(false)
    }];
    let mut ops_const = vec![quote!(false)];
    let mut label = None;
    let mut label_fields = Vec::new();
    let mut path_fields = Vec::new();
    let mut path_inits = Vec::new();
    let mut path_clones = Vec::new();
    let mut collection_names = Vec::new();
    let mut outbox_names = Vec::new();
    let owner = snake_case(&name.to_string()).replace('_', "-");
    for field in &fields.named {
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new(field.span(), "named field without a name"))?;
        let ty = &field.ty;
        let field_vis = &field.vis;
        let options = field_options(&field.attrs)?;
        let key = options.rename.clone().unwrap_or_else(|| ident.to_string());
        if !options.skip {
            keys.push((ident.to_string(), key.clone()));
        }
        if options.skip {
            // The containing type's default when it has one, which may
            // differ from the field type's (`is_admin: true`).
            builds.push(quote!(#ident: {
                #[allow(unused_imports, reason = "one of the two applies")]
                use #krate::__private::{HasDefault as _, NoDefault as _};
                (&#krate::__private::DefaultOf::<Self>::new())
                    .default_value()
                    .map_or_else(::core::default::Default::default, |value: Self| value.#ident)
            }));
            continue;
        }
        let index = proc_macro2::Literal::usize_unsuffixed(descs.len());
        let field_name = ident.to_string();
        let title = doc_lines(&field.attrs)
            .into_iter()
            .find(|line| !line.is_empty())
            .map_or_else(
                || humanize(&field_name),
                |line| line.trim_end_matches('.').to_owned(),
            );
        // An outbox is its own storage unit: what a change queues in it is
        // what reaches outside the record, and a receiver tells that from
        // the units the change wrote without the app's code.
        let outbox = last_ident(ty).is_some_and(|(name, _)| name == "Outbox");
        let (unit, readonly, is_label) = (options.unit || outbox, options.readonly, options.label);
        let label_part = label_parts
            .iter()
            .position(|part| *part == field_name)
            .map_or(0_u8, |position| {
                u8::try_from(position).unwrap_or(u8::MAX).saturating_add(1)
            });
        if label_part > 0 {
            label_fields.push((label_part, descs.len()));
        }
        let quiet_remove = options.quiet_remove;
        let trust = options.trust(&krate)?;
        if is_label {
            if label.is_some() {
                return Err(syn::Error::new(
                    field.span(),
                    "one field per type is its `#[model(label)]`",
                ));
            }
            label = Some(descs.len());
        }
        let mut keyed_shape = None;
        let field_table = if options.atom {
            quote!(&#table::ATOM_STRING)
        } else if options.keyed {
            let Some(item) = vec_item(ty) else {
                return Err(syn::Error::new(
                    field.span(),
                    "`#[model(keyed)]` is for a `Vec<T>` whose `T` carries an id",
                ));
            };
            keyed_shape = Some(quote!(#krate::Shape::Keyed(&<#item as #krate::Model>::SHAPE)));
            quote!(&#krate::KeyedVecOf::<#item>::TABLE)
        } else {
            quote!(<#ty as #krate::Model>::TABLE)
        };
        if outbox {
            outbox_names.push(field_name.clone());
        }
        let keyed = last_ident(ty).is_some_and(|(name, _)| name == "Keyed" || name == "Ordered");
        let items = if keyed || last_ident(ty).is_some_and(|(name, _)| name == "Collection") {
            collection_names.push(field_name.clone());
            quote!(::core::option::Option::Some(&#table::ItemOpsOf::<#ty>::OPS))
        } else {
            quote!(::core::option::Option::None)
        };
        let whole = options.whole;
        descs.push(quote!(#table::FieldDesc {
            key: #key,
            name: #field_name,
            table: #field_table,
            readonly: #readonly,
            whole: #whole,
            items: #items,
        }));
        builds.push(if let Some(default) = &options.default {
            quote!(#ident: #table::take_or::<#ty>(source, || #default)?)
        } else {
            quote!(#ident: #table::take::<#ty>(source)?)
        });
        reach.push(quote!(#index => ::core::option::Option::Some(&value.#ident),));
        reach_mut.push(quote!(#index => ::core::option::Option::Some(&mut value.#ident),));
        checks_const.push(quote!(<#ty as #krate::Model>::CHECKS));
        ops_const.push(quote!(<#ty as #krate::Model>::OPS));
        let resource = if options.resource {
            quote!(::core::option::Option::Some(#krate::child::NestedResource::of::<#ty>()))
        } else {
            quote!(::core::option::Option::None)
        };
        let ty_text = quote!(#ty)
            .to_string()
            .replace(" < ", "<")
            .replace("< ", "<")
            .replace(" <", "<")
            .replace(" >", ">")
            .replace(" :: ", "::")
            .replace(":: ", "::")
            .replace(" ,", ",");
        let keyed_shape_or_type =
            keyed_shape.unwrap_or_else(|| quote!(<#ty as #krate::Model>::SHAPE));
        shapes.push(quote!(#krate::FieldShape {
            key: #key,
            owner: #owner,
            name: #field_name,
            ty: #ty_text,
            title: #title,
            shape: #keyed_shape_or_type,
            unit: #unit,
            readonly: #readonly,
            label: #is_label,
            label_part: #label_part,
            quiet_remove: #quiet_remove,
            trust: #trust,
            schema: <#ty as #krate::Model>::value_schema,
            text: #krate::stored_text::<#ty>,
            absent: #krate::absent_scalar::<#ty>,
            resource: #resource,
        }));
        path_fields.push(quote!(#field_vis #ident: <#ty as #krate::Model>::Paths<__R>));
        path_inits.push(quote!(#ident: <#ty as #krate::Model>::paths_at(
            path.child(#krate::Segment::Field(::std::string::String::from(#key)))
        )));
        path_clones.push(quote!(#ident: ::core::clone::Clone::clone(&self.#ident)));
    }
    if label_fields.len() != label_parts.len() {
        return Err(syn::Error::new(
            Span::call_site(),
            format!(
                "`#[model(label = \"..\")]` names {} field(s) `{name}` does not have",
                label_parts.len().saturating_sub(label_fields.len())
            ),
        ));
    }
    let key_of = |field: &String| {
        keys.iter()
            .find(|(name, _)| name == field)
            .map(|(_, key)| key.clone())
            .ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    format!("`{name}` has no stored field `{field}` for its check to read"),
                )
            })
    };
    let check_reads = match check_reads {
        Some(fields) => {
            let fields = fields.iter().map(key_of).collect::<syn::Result<Vec<_>>>()?;
            let items = check_items
                .iter()
                .map(key_of)
                .collect::<syn::Result<Vec<_>>>()?;
            quote!(::core::option::Option::Some(#table::CheckReads {
                fields: &[#(#fields),*],
                items: &[#(#items),*],
            }))
        }
        None if !check_items.is_empty() => {
            let items = check_items
                .iter()
                .map(key_of)
                .collect::<syn::Result<Vec<_>>>()?;
            quote!(::core::option::Option::Some(#table::CheckReads {
                fields: &[],
                items: &[#(#items),*],
            }))
        }
        None => quote!(::core::option::Option::None),
    };
    label_fields.sort_by_key(|(part, _)| *part);
    let label_positions: Vec<proc_macro2::Literal> = label_fields
        .iter()
        .map(|(_, position)| proc_macro2::Literal::usize_unsuffixed(*position))
        .collect();
    let label_index = label.map_or_else(
        || quote!(::core::option::Option::None),
        |position| {
            let position = proc_macro2::Literal::usize_unsuffixed(position);
            quote!(::core::option::Option::Some(#position))
        },
    );
    let label_fn = if label.is_none() && label_fields.is_empty() {
        quote!()
    } else {
        quote! {
            /// The label: the texts of its label fields joined by spaces,
            /// else its `#[model(label)]` field.
            #[inline]
            fn label(&self) -> ::core::option::Option<::std::string::String> {
                #table::label(&Self::__RECORD, self)
            }
        }
    };
    let (own_check, own_check_fns) = check.as_ref().map_or_else(
        || (quote!(::core::option::Option::None), quote!()),
        |path| {
            (
                quote!(::core::option::Option::Some(#table::OwnCheck {
                    check: Self::__own_check,
                    read: Self::__read_record,
                    reads: #check_reads,
                })),
                quote! {
                    fn __own_check(
                        value: &dyn ::core::any::Any,
                    ) -> ::core::result::Result<(), #krate::Refusal> {
                        match value.downcast_ref::<Self>() {
                            ::core::option::Option::Some(value) => {
                                #krate::__private::check_outcome(#path(value))
                            }
                            ::core::option::Option::None => ::core::result::Result::Ok(()),
                        }
                    }

                    fn __read_record(
                        doc: &#krate::Doc,
                        obj: &#krate::__private::ObjId,
                    ) -> ::core::result::Result<
                        ::std::boxed::Box<dyn ::core::any::Any>,
                        #krate::ModelError,
                    > {
                        ::core::result::Result::Ok(::std::boxed::Box::new(
                            <Self as #krate::Fields>::read_fields(doc, obj)?,
                        ))
                    }
                },
            )
        },
    );
    let collections = if collection_names.is_empty() {
        quote!()
    } else {
        quote! {
            #[inline]
            fn item_collections(&self) -> ::std::vec::Vec<#krate::items::ItemCollection> {
                #table::item_collections(&Self::__RECORD, self)
            }

            #[inline]
            fn item_collection_sizes(&self) -> ::std::vec::Vec<(&'static str, usize)> {
                #table::item_collection_sizes(&Self::__RECORD, self)
            }

            #[inline]
            fn item_label(
                &self,
                field: &str,
                id: &#krate::Id,
            ) -> ::core::option::Option<::std::string::String> {
                #table::item_label(&Self::__RECORD, self, field, id)
            }

            #[inline]
            fn item_search(&self) -> ::std::vec::Vec<#krate::items::ItemText> {
                #table::item_search(&Self::__RECORD, self)
            }
        }
    };
    let nested_fn = if collection_names.is_empty() {
        quote!()
    } else {
        quote! {
            #[inline]
            fn nested_collections(&self) -> ::std::vec::Vec<#krate::items::ItemCollection> {
                #table::item_collections(&Self::__RECORD, self)
            }
        }
    };
    let reach_value = if reach.is_empty() {
        quote!(let _ = value;)
    } else {
        quote!(let value = value.downcast_ref::<Self>()?;)
    };
    let reach_value_mut = if reach.is_empty() {
        quote!(let _ = value;)
    } else {
        quote!(let value = value.downcast_mut::<Self>()?;)
    };
    let paths_doc = format!("The typed paths into a [`{name}`], one per field.");
    let paths_fn_doc = format!("The typed paths into a [`{name}`], rooted at it.");
    Ok(quote! {
        impl #impl_generics #name #type_generics #where_clause {
            /// The record as data: its fields' table and how to reach them.
            #[doc(hidden)]
            pub const __RECORD: #table::RecordDesc = #table::RecordDesc {
                fields: &[#(#descs),*],
                field: Self::__field,
                field_mut: Self::__field_mut,
                build: Self::__build,
                empty: #table::empty_of::<Self>,
                unwrap: #table::unwrap_of::<Self>,
                replace: #table::replace_of::<Self>,
                own_check: #own_check,
                label: #label_index,
                label_parts: &[#(#label_positions),*],
                name: ::core::any::type_name::<Self>,
                shape: <Self as #krate::Model>::SHAPE,
                checks: <Self as #krate::Model>::CHECKS,
                has_ops: <Self as #krate::Model>::OPS,
            };

            #[allow(clippy::match_single_binding, reason = "a record may have no stored field")]
            fn __field(
                value: &dyn ::core::any::Any,
                index: usize,
            ) -> ::core::option::Option<&dyn ::core::any::Any> {
                #reach_value
                match index {
                    #(#reach)*
                    _ => ::core::option::Option::None,
                }
            }

            #[allow(clippy::match_single_binding, reason = "a record may have no stored field")]
            fn __field_mut(
                value: &mut dyn ::core::any::Any,
                index: usize,
            ) -> ::core::option::Option<&mut dyn ::core::any::Any> {
                #reach_value_mut
                match index {
                    #(#reach_mut)*
                    _ => ::core::option::Option::None,
                }
            }

            fn __build(
                source: &mut dyn #table::FieldSource,
                out: &mut dyn ::core::any::Any,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let _ = &source;
                #table::put(out, Self { #(#builds,)* })
            }

            #own_check_fns

            /// The fields holding a `Collection`, by name: where actions on
            /// an item can act.
            #[doc(hidden)]
            pub const __COLLECTIONS: &'static [&'static str] = &[#(#collection_names),*];

            /// Whether the type holds an invariant of its own
            /// (`#[model(check = path)]`), which a change of one item
            /// cannot be checked against without the whole value.
            #[doc(hidden)]
            pub const __OWN_CHECK: bool = #own_check_const;

            /// The fields holding an `Outbox`, by name: verbs may act on an
            /// entry of one (`#[action(item = field)]`).
            #[doc(hidden)]
            pub const __OUTBOXES: &'static [&'static str] = &[#(#outbox_names),*];

            #[doc = #paths_fn_doc]
            #vis fn paths() -> #paths_name<Self, #(#generic_args),*> {
                <Self as #krate::Model>::paths_at(#krate::Path::root())
            }
        }

        impl #impl_generics #krate::Model for #name #type_generics #where_clause {
            const SHAPE: #krate::Shape = #krate::Shape::Record(&[#(#shapes),*]);
            const CHECKS: bool = #(#checks_const)||*;
            const OPS: bool = #(#ops_const)||*;
            const TABLE: &'static #table::ModelTable = &#table::ModelTable::of_record(&Self::__RECORD);
            type Paths<__R> = #paths_name<__R, #(#generic_args),*>;

            fn paths_at<__R>(path: #krate::Path<__R, Self>) -> Self::Paths<__R> {
                #paths_name {
                    #(#path_inits,)*
                    __path: path,
                }
            }

            #[inline]
            fn to_json(&self) -> #krate::__private::serde_json::Value {
                #table::to_json(&Self::__RECORD, self)
            }

            #[inline]
            fn from_json(
                value: &#krate::__private::serde_json::Value,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let mut out = ::core::option::Option::None;
                #table::from_json(&Self::__RECORD, value, &mut out)?;
                #table::filled(out)
            }

            #[inline]
            fn edit(
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
                path: &[#krate::Segment],
                op: &#krate::EditOp,
                cx: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::edit(&Self::__RECORD, doc, slot, path, op, cx)
            }

            #[inline]
            fn check(&self) -> ::core::result::Result<(), #krate::Refusal> {
                #table::check(&Self::__RECORD, self)
            }

            #label_fn

            #[inline]
            fn op_coalesces(
                path: &[#krate::Segment],
                previous: &#krate::__private::serde_json::Value,
                next: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<bool> {
                #table::op_coalesces(&Self::__RECORD, path, previous, next)
            }

            #[inline]
            fn op_continuable(
                path: &[#krate::Segment],
                op: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<bool> {
                #table::op_continuable(&Self::__RECORD, path, op)
            }

            #[inline]
            fn diff<__E>(
                &self,
                after: &Self,
                path: &[#krate::Segment],
                out: &mut ::std::vec::Vec<#krate::Edit<__E>>,
            ) {
                #table::diff_into(&Self::__RECORD, self, after, path, out);
            }

            #[inline]
            fn op_describe(
                path: &[#krate::Segment],
                op: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<#krate::path::OpDescription> {
                #table::op_describe(&Self::__RECORD, path, op)
            }

            #[inline]
            fn op_lands(
                &self,
                path: &[#krate::Segment],
                op: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<#krate::Place> {
                #table::op_lands(&Self::__RECORD, self, path, op)
            }

            #[inline]
            fn op_schemas(
                prefix: &str,
                out: &mut ::std::vec::Vec<(::std::string::String, #krate::__private::serde_json::Value)>,
            ) {
                #table::op_schemas(&Self::__RECORD, prefix, out);
            }

            #[inline]
            fn get_at(
                &self,
                path: &[#krate::Segment],
            ) -> ::core::option::Option<&dyn ::core::any::Any> {
                #table::get_at(&Self::__RECORD, self, path)
            }

            #[inline]
            fn load(
                doc: &#krate::Doc,
                value: &#krate::__private::Value<'_>,
                id: &#krate::__private::ObjId,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let mut out = ::core::option::Option::None;
                #table::load(&Self::__RECORD, doc, value, id, &mut out)?;
                #table::filled(out)
            }
            #nested_fn

            #[inline(never)]
            fn value_schema() -> #krate::__private::serde_json::Value {
                #krate::__private::record_value_schema(<Self as #krate::Model>::SHAPE)
            }

            #[inline]
            fn missing() -> ::core::result::Result<Self, #krate::ModelError> {
                let mut out = ::core::option::Option::None;
                #table::missing(&Self::__RECORD, &mut out)?;
                #table::filled(out)
            }

            #[inline]
            fn store(
                &self,
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::store(&Self::__RECORD, self, doc, slot)
            }

            #[inline]
            fn stores_as(&self, before: &Self) -> bool {
                #table::stores_as(&Self::__RECORD, self, before)
            }

            #[inline]
            fn store_changed(
                &self,
                before: &Self,
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
                dirty: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::store_changed(&Self::__RECORD, self, before, doc, slot, dirty)
            }

            #[inline]
            fn refresh(
                &mut self,
                doc: &#krate::Doc,
                value: &#krate::__private::Value<'_>,
                id: &#krate::__private::ObjId,
                delta: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::refresh(&Self::__RECORD, self, doc, value, id, delta)
            }

            #[inline]
            fn collect_text(&self, out: &mut ::std::vec::Vec<::std::string::String>) {
                #table::collect_text(&Self::__RECORD, self, out);
            }
        }

        impl #impl_generics #krate::Settable for #name #type_generics #where_clause {}

        impl #impl_generics #krate::Fields for #name #type_generics #where_clause {
            const ITEM_FIELDS: &'static [&'static str] = Self::__COLLECTIONS;

            #[inline]
            fn read_fields(
                doc: &#krate::Doc,
                obj: &#krate::__private::ObjId,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let mut out = ::core::option::Option::None;
                #table::read_fields(&Self::__RECORD, doc, obj, &mut out)?;
                #table::filled(out)
            }

            #[inline]
            fn write_fields(
                &self,
                doc: &mut #krate::Doc,
                obj: &#krate::__private::ObjId,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::write_fields(&Self::__RECORD, self, doc, obj)
            }

            #[inline]
            fn write_changed_fields(
                &self,
                before: &Self,
                doc: &mut #krate::Doc,
                obj: &#krate::__private::ObjId,
                dirty: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::write_changed_fields(&Self::__RECORD, self, before, doc, obj, dirty)
            }

            #[inline]
            fn edit_fields(
                doc: &mut #krate::Doc,
                obj: &#krate::__private::ObjId,
                path: &[#krate::Segment],
                op: &#krate::EditOp,
                cx: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::edit_fields(&Self::__RECORD, doc, obj, path, op, cx)
            }

            #collections

            #[inline]
            fn refresh_fields(
                &mut self,
                doc: &#krate::Doc,
                obj: &#krate::__private::ObjId,
                delta: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #table::refresh_fields(&Self::__RECORD, self, doc, obj, delta)
            }
        }

        #[doc = #paths_doc]
        #vis struct #paths_name #paths_impl_generics #where_clause {
            #(#path_fields,)*
            __path: #krate::Path<__R, #name #type_generics>,
        }

        impl #paths_impl_generics ::core::clone::Clone for #paths_name #paths_type_generics #where_clause {
            fn clone(&self) -> Self {
                Self {
                    #(#path_clones,)*
                    __path: ::core::clone::Clone::clone(&self.__path),
                }
            }
        }

        impl #paths_impl_generics #paths_name #paths_type_generics #where_clause {
            /// The path of the whole value.
            #vis fn path(&self) -> &#krate::Path<__R, #name #type_generics> {
                &self.__path
            }
        }
    })
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn humanize(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn doc_lines(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            Meta::NameValue(MetaNameValue {
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(text),
                        ..
                    }),
                ..
            }) => Some(text.value().trim().to_owned()),
            _ => None,
        })
        .collect()
}

fn last_ident(ty: &Type) -> Option<(String, Option<&Type>)> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    let inner = match &segment.arguments {
        PathArguments::AngleBracketed(args) => args.args.iter().find_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        }),
        _ => None,
    };
    Some((segment.ident.to_string(), inner))
}

/// How a parameter of Rust type `ty` holds bytes, when it does (`Vec<u8>`,
/// or a list of them): the decoder that takes them inline or by a content
/// reference, and the schema saying both. Actions carry intent and
/// references; bytes travel by session transport into the content store.
fn byte_param(ty: &Type) -> Option<(Tokens, Tokens)> {
    let krate = krate();
    let is_bytes = |ty: &Type| {
        last_ident(ty).is_some_and(|(name, inner)| {
            name == "Vec"
                && inner
                    .and_then(last_ident)
                    .is_some_and(|(byte, _)| byte == "u8")
        })
    };
    let (name, inner) = last_ident(ty)?;
    if is_bytes(ty) {
        return Some((
            quote!(#krate::content::bytes),
            quote!(#krate::content::bytes_schema),
        ));
    }
    (name == "Vec" && inner.is_some_and(is_bytes)).then(|| {
        (
            quote!(#krate::content::byte_lists),
            quote!(#krate::content::byte_list_schema),
        )
    })
}

/// The declared type of a parameter of Rust type `ty`, and whether it is
/// optional. An app's own type is what its schema says it is written as
/// when `schema` (the type implements `JsonSchema`), else an object.
/// The parameter type of `ty` when its name says it, and whether it is
/// optional; `None` for a type whose schema says it.
fn known_param_type(ty: &Type) -> (Option<Tokens>, bool) {
    let krate = krate();
    let pt = quote!(#krate::__private::ParamType);
    let (name, inner) = last_ident(ty).unwrap_or_default();
    match name.as_str() {
        "Option" => inner.map_or_else(
            || (Some(quote!(#pt::Object)), true),
            |inner| (known_param_type(inner).0, true),
        ),
        "String" | "Id" | "Uuid" | "Atom" => (Some(quote!(#pt::Text)), false),
        "f32" | "f64" => (Some(quote!(#pt::Number)), false),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            (Some(quote!(#pt::Integer)), false)
        }
        "bool" => (Some(quote!(#pt::Boolean)), false),
        "Vec" => (Some(quote!(#pt::Array)), false),
        _ => (None, false),
    }
}

/// The type whose schema says the parameter type of `ty`: the present value
/// of an `Option`, else `ty`.
fn schema_param_type(ty: &Type) -> &Type {
    match last_ident(ty) {
        Some((name, Some(inner))) if name == "Option" => schema_param_type(inner),
        _ => ty,
    }
}

fn param_type(ty: &Type, schema: bool) -> (Tokens, bool) {
    let krate = krate();
    let (known, optional) = known_param_type(ty);
    if let Some(known) = known {
        return (known, optional);
    }
    if let Some((name, Some(inner))) = last_ident(ty)
        && name == "Option"
    {
        return (param_type(inner, schema).0, true);
    }
    if schema {
        (
            quote!(#krate::__private::param_type_of(
                &<#ty as #krate::JsonSchema>::json_schema()
            )),
            false,
        )
    } else {
        // An effect's field need not describe its JSON form: its schema when
        // it does (an alias of a described type), an object when not.
        (
            quote!({
                #[allow(unused_imports, reason = "one of the two applies")]
                use #krate::__private::{DescribedParam as _, UndescribedParam as _};
                (&#krate::__private::ParamOf::<#ty>::new()).param_type()
            }),
            false,
        )
    }
}

/// The owned type an argument is carried as in the action payload, and
/// whether the method borrows it (`&str` is carried as `String`, `&[T]` as
/// `Vec<T>`, `&T` as `T`).
fn owned_type(ty: &Type) -> (Type, bool) {
    let Type::Reference(reference) = ty else {
        return (ty.clone(), false);
    };
    let owned = match &*reference.elem {
        Type::Path(path) if path.path.is_ident("str") => syn::parse_quote!(::std::string::String),
        Type::Slice(slice) => {
            let elem = &slice.elem;
            syn::parse_quote!(::std::vec::Vec<#elem>)
        }
        elem => elem.clone(),
    };
    (owned, true)
}

fn is_action_cx(ty: &Type) -> bool {
    let Type::Reference(reference) = ty else {
        return false;
    };
    reference.mutability.is_some()
        && last_ident(&reference.elem).is_some_and(|(name, _)| name == "ActionCx")
}

/// Turn a record's `#[action]` methods into its replayable, published action
/// vocabulary, and implement `Resource` for it.
///
/// ```ignore
/// #[resource(kind = "example.note", label = "Note")]
/// impl Note {
///     /// Replace the note's text.
///     #[action]
///     pub fn edit(&mut self, #[param("Note text")] text: String) { self.body = text; }
/// }
/// ```
///
/// Generates, per action, a parameter struct in a module named after the
/// type (`note::Edit { text }`, with a JSON Schema derived from the
/// parameter types), a variant of `NoteAction` carrying it, and
/// `Note::EDIT`, an `ActionRef` views bind controls to and tests apply;
/// then `impl Resource`. Field edits are a built-in variant.
///
/// An action method takes `&mut self`, optionally `cx: &mut ActionCx` and
/// `&mut Outbox<T>` fields of the record (by field name: such an action
/// reaches outside the document), then its parameters, and returns `()` or
/// `Result<(), impl Into<Refusal>>`. Its public id is its name in kebab case
/// unless `#[action(id = "...")]`; a parameter is published under its name
/// unless `#[param(name = "...")]`, and `#[param("Description", example =
/// value)]` describes it.
///
/// An action whose first parameter is an item `Id` is also published on each
/// item of the collection it acts on, with the id bound: the collection named
/// like the parameter, or the record's only one; `#[action(item = field)]`
/// names it otherwise, and anything else does not compile.
///
/// Action options: `label = "..."`, `destructive` (it deletes or overwrites:
/// when Undo takes it back, as it does any change of the document alone, it
/// runs at once and offers Undo; otherwise people confirm it, through any
/// door), `irreversible` (it also loses what Undo cannot bring back, such
/// as a secret every device forgets: a destructive one is confirmed even
/// though Undo reverts its document change), `housekeeping` (a change
/// about the record rather than to it,
/// such as a read marker), `external` (content from outside, nobody's undo
/// step), `local` (an action of a record with a `queue` that queues nothing,
/// so an undoable change rather than one reaching outside), `field = "..."` (an action that writes only that field is placed
/// below it, so a grant on the field reaches it: someone who may only
/// comment may add to the `comments`).
///
/// Resource options: `label`, `params = module` (the parameter module's
/// name), `initialize = path` (`fn() -> Self`, the deterministic genesis),
/// `seed = path` (`fn(&mut Self, &mut ActionCx)`, the creator's first
/// content), `available = path` (`fn(&Self, module::Verb) -> Availability`),
/// `children = path` (`fn(&Self) -> Vec<Derived<Self>>`), `search = path`,
/// `scopes = path`, `read_content = path` (`fn(&Self, Option<&Id>) ->
/// Result<serde_json::Value, String>`), `read_content_in_doc = path` (the same
/// read, `fn(&Self, &Doc, Option<&Id>) -> ...`, for one that needs the
/// document itself), `facts = path` (`fn(&Self) -> Result<serde_json::Value,
/// String>`, the compact summary `Resource::facts` states) and `views = path`.
#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let options = match verbs::resource_options(attr) {
        Ok(options) => options,
        Err(error) => return error.to_compile_error().into(),
    };
    let mut item = parse_macro_input!(item as ItemImpl);
    match verbs::expand(&options, &mut item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

mod verbs;
