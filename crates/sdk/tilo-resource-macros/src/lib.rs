//! Procedural macros for `tilo-resource`. See that crate for the API; this
//! crate only generates calls into it.

use proc_macro::TokenStream;
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
///   marked `#[default]` is the value of an absent slot.
///
/// Field attributes: `#[model(rename = "key")]`, `#[model(skip)]` (not
/// stored; the containing type's `Default` on read), `#[model(atom)]` on a
/// `String` (written whole, last writer wins, as `Atom` is).
///
/// A struct's `Model::SHAPE` lists its stored fields, so layout-aware services
/// (History restore) reconcile it field by field.
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

mod schema;

/// Derive `Effect`: an enum's variants become the app's published effects —
/// work with no document delta, invoked by buttons, the palette, agents and
/// scripts alike. See `tilo_resource::effect`.
///
/// `#[effect(operation = "app.effects")]` names the vocabulary's operation
/// type; `device_only` beside it marks every effect device-only. Each variant is unit or has named fields (the effect's typed
/// parameters, `#[param("…")]` describing one); its id is the variant name in
/// kebab case and its label the first doc line. Variant options:
/// `#[effect(label = "...", view, row, job, cancellable, returns,
/// destructive, privileged, idempotent, device_only, programmatic, untrusted, risk = "path", permission = "read")]`.
/// A `device_only` effect reaches the files, hardware or services of the
/// device it runs on: only the person at that device may invoke it. In a
/// vocabulary marked `device_only`, `#[effect(device_only = false)]` exempts
/// one effect that reaches nothing of the device (a personal setting). Generates `impl Effect` and one `ActionRef`
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
            } else {
                return Err(meta.error(
                    "expected `skip`, `rename = \"...\"`, `label`, `readonly`, `unit` or `atom`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(options)
}

/// `#[model(check = path)]` on the type.
fn container_check(attrs: &[Attribute]) -> syn::Result<Option<syn::Path>> {
    let mut check = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("model")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("check") {
                check = Some(meta.value()?.parse::<syn::Path>()?);
                Ok(())
            } else {
                Err(meta.error("expected `check = path` on the type"))
            }
        })?;
    }
    Ok(check)
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
        Data::Enum(data) => {
            let mut names = Vec::new();
            let mut idents = Vec::new();
            let mut default = None;
            for variant in &data.variants {
                if !matches!(variant.fields, Fields::Unit) {
                    return Err(syn::Error::new(
                        variant.span(),
                        "Model derives for enums of unit variants; store data enums with `Json<T>`",
                    ));
                }
                let options = field_options(&variant.attrs)?;
                let key = options
                    .rename
                    .unwrap_or_else(|| snake_case(&variant.ident.to_string()));
                if variant
                    .attrs
                    .iter()
                    .any(|attr| attr.path().is_ident("default"))
                {
                    default = Some(variant.ident.clone());
                }
                names.push(key);
                idents.push(variant.ident.clone());
            }
            let missing = default.map_or_else(
                || {
                    quote!(::core::result::Result::Err(#krate::ModelError::Missing {
                        path: ::std::string::String::new()
                    }))
                },
                |ident| quote!(::core::result::Result::Ok(Self::#ident)),
            );
            Ok(quote! {
                impl #impl_generics #krate::Settable for #name #type_generics #where_clause {}

                impl #impl_generics #krate::Model for #name #type_generics #where_clause {
                    type Paths<__R> = #krate::Path<__R, Self>;

                    fn paths_at<__R>(path: #krate::Path<__R, Self>) -> #krate::Path<__R, Self> {
                        path
                    }
                    fn to_json(&self) -> #krate::__private::serde_json::Value {
                        let text = match self { #(Self::#idents => #names,)* };
                        #krate::__private::serde_json::Value::String(::std::string::String::from(text))
                    }
                    fn from_json(
                        value: &#krate::__private::serde_json::Value,
                    ) -> ::core::result::Result<Self, #krate::ModelError> {
                        match value {
                            #krate::__private::serde_json::Value::String(text) => match text.as_str() {
                                #(#names => ::core::result::Result::Ok(Self::#idents),)*
                                other => ::core::result::Result::Err(#krate::ModelError::invalid(
                                    ::std::format!("unknown variant `{other}`"),
                                )),
                            },
                            #krate::__private::serde_json::Value::Null => <Self as #krate::Model>::missing(),
                            _ => ::core::result::Result::Err(#krate::ModelError::shape("a variant name")),
                        }
                    }
                    fn load(
                        doc: &#krate::Doc,
                        value: &#krate::__private::Value<'_>,
                        id: &#krate::__private::ObjId,
                    ) -> ::core::result::Result<Self, #krate::ModelError> {
                        let #krate::Atom(text) = <#krate::Atom as #krate::Model>::load(doc, value, id)?;
                        match text.as_str() {
                            #(#names => ::core::result::Result::Ok(Self::#idents),)*
                            other => ::core::result::Result::Err(#krate::ModelError::invalid(
                                ::std::format!("unknown variant `{other}`"),
                            )),
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
                        let text = match self { #(Self::#idents => #names,)* };
                        slot.put_scalar(doc, #krate::__private::ScalarValue::Str(text.into()))
                    }
                    fn collect_text(&self, out: &mut ::std::vec::Vec<::std::string::String>) {
                        let text = match self { #(Self::#idents => #names,)* };
                        out.push(::std::string::String::from(text));
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
    reason = "one linear expansion reads better than helpers threading a dozen token lists"
)]
fn derive_named(input: &DeriveInput, fields: &syn::FieldsNamed) -> syn::Result<Tokens> {
    let krate = krate();
    let name = &input.ident;
    let vis = &input.vis;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let check = container_check(&input.attrs)?;
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

    let mut reads = Vec::new();
    let mut missing = Vec::new();
    let mut writes = Vec::new();
    let mut refreshes = Vec::new();
    let mut texts = Vec::new();
    let mut shapes = Vec::new();
    let mut collections = Vec::new();
    let mut path_fields = Vec::new();
    let mut path_inits = Vec::new();
    let mut path_clones = Vec::new();
    let mut to_json = Vec::new();
    let mut from_json = Vec::new();
    let mut edit_arms = Vec::new();
    let mut checks = Vec::new();
    let mut checks_const = vec![if check.is_some() {
        quote!(true)
    } else {
        quote!(false)
    }];
    let mut label = None;
    let mut readonly_fields = Vec::new();
    let mut get_arms = Vec::new();
    let mut op_coalesce_arms = Vec::new();
    let mut op_continuable_arms = Vec::new();
    let mut op_lands_arms = Vec::new();
    let mut op_describe_arms = Vec::new();
    let mut diffs = Vec::new();
    let mut op_schemas = Vec::new();
    let mut collection_names = Vec::new();
    let mut item_label_arms = Vec::new();
    let mut item_searches = Vec::new();
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
        if options.skip {
            // The containing type's default when it has one, which may
            // differ from the field type's (`is_admin: true`).
            let default = quote!({
                #[allow(unused_imports, reason = "one of the two applies")]
                use #krate::__private::{HasDefault as _, NoDefault as _};
                (&#krate::__private::DefaultOf::<Self>::new())
                    .default_value()
                    .map_or_else(::core::default::Default::default, |value: Self| value.#ident)
            });
            reads.push(quote!(#ident: #default));
            missing.push(quote!(#ident: #default));
            from_json.push(quote!(#ident: #default));
            continue;
        }
        let field_name = ident.to_string();
        let title = doc_lines(&field.attrs)
            .into_iter()
            .find(|line| !line.is_empty())
            .map_or_else(
                || humanize(&field_name),
                |line| line.trim_end_matches('.').to_owned(),
            );
        let (unit, readonly, is_label) = (options.unit, options.readonly, options.label);
        if is_label {
            if label.is_some() {
                return Err(syn::Error::new(
                    field.span(),
                    "one field per type is its `#[model(label)]`",
                ));
            }
            label = Some(ident.clone());
        }
        if options.atom {
            // A `String` written whole, last writer winning: an id or a
            // name two people setting at once must not interleave.
            let atom = quote!(#krate::Atom);
            reads.push(quote!(#ident: #krate::read_key::<#atom>(doc, obj, #key)?.0));
            missing.push(quote!(
                #ident: <#atom as #krate::Model>::missing().map_err(|error| error.at(#key))?.0
            ));
            writes.push(quote!(#krate::write_key::<#atom>(
                doc,
                obj,
                #key,
                &#krate::Atom(::core::clone::Clone::clone(&self.#ident)),
            )?;));
            refreshes.push(quote! {
                let mut atom = #krate::Atom(::core::mem::take(&mut self.#ident));
                let refreshed = #krate::refresh_key::<#atom>(&mut atom, doc, obj, #key, delta);
                self.#ident = atom.0;
                refreshed?;
            });
        } else {
            reads.push(quote!(#ident: #krate::read_key::<#ty>(doc, obj, #key)?));
            missing.push(quote!(
                #ident: <#ty as #krate::Model>::missing().map_err(|error| error.at(#key))?
            ));
            writes.push(quote!(#krate::write_key::<#ty>(doc, obj, #key, &self.#ident)?;));
            refreshes.push(
                quote!(#krate::refresh_key::<#ty>(&mut self.#ident, doc, obj, #key, delta)?;),
            );
        }
        texts.push(quote!(#krate::Model::collect_text(&self.#ident, out);));
        get_arms.push(quote!(#key => #krate::Model::get_at(&self.#ident, rest),));
        op_coalesce_arms
            .push(quote!(#key => <#ty as #krate::Model>::op_coalesces(rest, previous, next),));
        op_continuable_arms.push(quote!(#key => <#ty as #krate::Model>::op_continuable(rest, op),));
        op_lands_arms.push(quote!(#key => #krate::Model::op_lands(&self.#ident, rest, op),));
        op_describe_arms.push(quote!(#key => <#ty as #krate::Model>::op_describe(rest, op),));
        if !readonly {
            diffs.push(quote! {
                let mut at = path.to_vec();
                at.push(#krate::Segment::Field(::std::string::String::from(#key)));
                #krate::Model::diff(&self.#ident, &after.#ident, &at, out);
            });
        }
        op_schemas.push(quote! {
            <#ty as #krate::Model>::op_schemas(&::std::format!("{prefix}{}.", #key), out);
        });
        let ty_text = quote!(#ty)
            .to_string()
            .replace(" < ", "<")
            .replace("< ", "<")
            .replace(" <", "<")
            .replace(" >", ">")
            .replace(" :: ", "::")
            .replace(":: ", "::")
            .replace(" ,", ",");
        shapes.push(quote!(#krate::FieldShape {
            key: #key,
            owner: #owner,
            name: #field_name,
            ty: #ty_text,
            title: #title,
            shape: <#ty as #krate::Model>::SHAPE,
            unit: #unit,
            readonly: #readonly,
            label: #is_label,
        }));
        path_fields.push(quote!(#field_vis #ident: <#ty as #krate::Model>::Paths<__R>));
        path_inits.push(quote!(#ident: <#ty as #krate::Model>::paths_at(
            path.child(#krate::Segment::Field(::std::string::String::from(#key)))
        )));
        path_clones.push(quote!(#ident: ::core::clone::Clone::clone(&self.#ident)));
        to_json.push(quote!(
            fields.insert(::std::string::String::from(#key), #krate::Model::to_json(&self.#ident));
        ));
        from_json.push(quote!(#ident: match fields.get(#key) {
            ::core::option::Option::Some(value) => <#ty as #krate::Model>::from_json(value),
            ::core::option::Option::None => <#ty as #krate::Model>::missing(),
        }
        .map_err(|error| error.at(#key))?));
        if readonly {
            readonly_fields.push(key.clone());
            edit_arms.push(quote!(#key => {
                return ::core::result::Result::Err(#krate::ModelError::Refused(
                    ::std::format!("`{}` is changed only by the record's actions", #field_name),
                ));
            }));
        } else if options.atom {
            edit_arms.push(quote!(#key => <#krate::Atom as #krate::Model>::edit(
                doc,
                #krate::Slot::Key(obj, #key),
                rest,
                op,
                cx,
            )
            .map_err(|error| error.at(#key))?,));
        } else {
            edit_arms.push(quote!(#key => <#ty as #krate::Model>::edit(
                doc,
                #krate::Slot::Key(obj, #key),
                rest,
                op,
                cx,
            )
            .map_err(|error| error.at(#key))?,));
        }
        checks.push(quote!(#krate::Model::check(&self.#ident)?;));
        checks_const.push(quote!(<#ty as #krate::Model>::CHECKS));
        let keyed = last_ident(ty).is_some_and(|(name, _)| name == "Keyed" || name == "Ordered");
        if keyed || last_ident(ty).is_some_and(|(name, _)| name == "Collection") {
            // Each item with its id: a collection's ids are its own, keyed
            // and ordered items carry theirs.
            let (items, find) = if keyed {
                (
                    quote!(self.#ident.iter().map(|item| (#krate::Identified::id(item), item))),
                    quote!(self.#ident.find(id)),
                )
            } else {
                (
                    quote!(self.#ident.iter().map(|(id, item)| (::core::clone::Clone::clone(id), item))),
                    quote!(self.#ident.get(id)),
                )
            };
            collection_names.push(field_name.clone());
            item_label_arms.push(quote!(#field_name => #find
                .map(|item| #krate::__private::item_label(id, item)),));
            item_searches.push(quote! {
                for (id, item) in #items {
                    if let ::core::option::Option::Some(label) = #krate::Model::label(item) {
                        let mut text = ::std::vec::Vec::new();
                        #krate::Model::collect_text(item, &mut text);
                        found.push(#krate::items::ItemText {
                            field: #field_name,
                            id,
                            label,
                            text: text.join("\n"),
                        });
                    }
                }
            });
            collections.push(quote!(#krate::items::ItemCollection {
                field: #field_name,
                items: #items
                    .take(#krate::items::ITEM_PAGE)
                    .map(|(id, item)| {
                        let label = #krate::__private::item_label(&id, item);
                        (id, label)
                    })
                    .collect(),
            }));
        }
    }
    let item_collections = if collections.is_empty() {
        quote!()
    } else {
        quote! {
            fn item_collections(&self) -> ::std::vec::Vec<#krate::items::ItemCollection> {
                ::std::vec![#(#collections),*]
            }

            fn item_label(
                &self,
                field: &str,
                id: &#krate::Id,
            ) -> ::core::option::Option<::std::string::String> {
                match field {
                    #(#item_label_arms)*
                    _ => ::core::option::Option::None,
                }
            }

            fn item_search(&self) -> ::std::vec::Vec<#krate::items::ItemText> {
                let mut found = ::std::vec::Vec::new();
                #(#item_searches)*
                found
            }
        }
    };
    let own_check = check.as_ref().map_or_else(
        || quote!(),
        |path| quote!(#krate::__private::check_outcome(#path(self))?;),
    );
    let check_after_edit = check.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                let current = <Self as #krate::Fields>::read_fields(doc, obj)?;
                #krate::__private::check_outcome(#path(&current)).map_err(#krate::ModelError::refused)?;
            }
        },
    );
    let whole_set = if readonly_fields.is_empty() {
        quote! {
            let value = <Self as #krate::Model>::from_json(value)?;
            #krate::Model::check(&value).map_err(#krate::ModelError::refused)?;
            <Self as #krate::Fields>::write_fields(&value, doc, obj)?;
        }
    } else {
        quote! {
            let _ = value;
            return ::core::result::Result::Err(#krate::ModelError::Refused(
                ::std::string::String::from("a record with read-only fields is not replaced whole"),
            ));
        }
    };
    let label_fn = label.as_ref().map_or_else(
        || quote!(),
        |ident| {
            quote! {
                fn label(&self) -> ::core::option::Option<::std::string::String> {
                    let mut text = ::std::vec::Vec::new();
                    #krate::Model::collect_text(&self.#ident, &mut text);
                    text.into_iter()
                        .map(|line| line.lines().next().unwrap_or_default().trim().to_owned())
                        .find(|line| !line.is_empty())
                }
            }
        },
    );
    let paths_doc = format!("The typed paths into a [`{name}`], one per field.");
    let paths_fn_doc = format!("The typed paths into a [`{name}`], rooted at it.");
    Ok(quote! {
        impl #impl_generics #krate::Model for #name #type_generics #where_clause {
            const SHAPE: #krate::Shape = #krate::Shape::Record(&[#(#shapes),*]);
            const CHECKS: bool = #(#checks_const)||*;
            type Paths<__R> = #paths_name<__R, #(#generic_args),*>;

            fn paths_at<__R>(path: #krate::Path<__R, Self>) -> Self::Paths<__R> {
                #paths_name {
                    #(#path_inits,)*
                    __path: path,
                }
            }

            fn to_json(&self) -> #krate::__private::serde_json::Value {
                let mut fields = #krate::__private::serde_json::Map::new();
                #(#to_json)*
                #krate::__private::serde_json::Value::Object(fields)
            }

            fn from_json(
                value: &#krate::__private::serde_json::Value,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let fields = match value {
                    #krate::__private::serde_json::Value::Object(fields) => fields,
                    #krate::__private::serde_json::Value::Null => {
                        return <Self as #krate::Model>::missing();
                    }
                    _ => return ::core::result::Result::Err(#krate::ModelError::shape("an object")),
                };
                let _ = fields;
                ::core::result::Result::Ok(Self { #(#from_json,)* })
            }

            fn edit(
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
                path: &[#krate::Segment],
                op: &#krate::EditOp,
                cx: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let obj = slot.object(doc, #krate::__private::ObjType::Map)?;
                <Self as #krate::Fields>::edit_fields(doc, &obj, path, op, cx)
            }

            fn check(&self) -> ::core::result::Result<(), #krate::Refusal> {
                if !<Self as #krate::Model>::CHECKS {
                    return ::core::result::Result::Ok(());
                }
                #own_check
                #(#checks)*
                ::core::result::Result::Ok(())
            }

            #label_fn

            fn op_coalesces(
                path: &[#krate::Segment],
                previous: &#krate::__private::serde_json::Value,
                next: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<bool> {
                let _ = (previous, next);
                match path.split_first() {
                    ::core::option::Option::Some((#krate::Segment::Field(key), rest)) => {
                        let _ = rest;
                        match key.as_str() {
                            #(#op_coalesce_arms)*
                            _ => ::core::option::Option::None,
                        }
                    }
                    _ => ::core::option::Option::None,
                }
            }

            fn op_continuable(
                path: &[#krate::Segment],
                op: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<bool> {
                let _ = op;
                match path.split_first() {
                    ::core::option::Option::Some((#krate::Segment::Field(key), rest)) => {
                        let _ = rest;
                        match key.as_str() {
                            #(#op_continuable_arms)*
                            _ => ::core::option::Option::None,
                        }
                    }
                    _ => ::core::option::Option::None,
                }
            }

            fn diff<__E>(
                &self,
                after: &Self,
                path: &[#krate::Segment],
                out: &mut ::std::vec::Vec<#krate::Edit<__E>>,
            ) {
                let _ = (after, path, &out);
                #(#diffs)*
            }

            fn op_describe(
                path: &[#krate::Segment],
                op: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<#krate::path::OpDescription> {
                let _ = op;
                match path.split_first() {
                    ::core::option::Option::Some((#krate::Segment::Field(key), rest)) => {
                        let _ = rest;
                        match key.as_str() {
                            #(#op_describe_arms)*
                            _ => ::core::option::Option::None,
                        }
                    }
                    _ => ::core::option::Option::None,
                }
            }

            fn op_lands(
                &self,
                path: &[#krate::Segment],
                op: &#krate::__private::serde_json::Value,
            ) -> ::core::option::Option<#krate::Place> {
                let _ = op;
                match path.split_first() {
                    ::core::option::Option::Some((#krate::Segment::Field(key), rest)) => {
                        let _ = rest;
                        match key.as_str() {
                            #(#op_lands_arms)*
                            _ => ::core::option::Option::None,
                        }
                    }
                    _ => ::core::option::Option::None,
                }
            }

            fn op_schemas(
                prefix: &str,
                out: &mut ::std::vec::Vec<(::std::string::String, #krate::__private::serde_json::Value)>,
            ) {
                // A type nested in itself has no vocabulary of its own
                // deeper than this.
                if prefix.matches('.').count() > 16 {
                    return;
                }
                let _ = &out;
                #(#op_schemas)*
            }

            fn get_at(
                &self,
                path: &[#krate::Segment],
            ) -> ::core::option::Option<&dyn ::core::any::Any>
            where
                Self: 'static,
            {
                match path.split_first() {
                    ::core::option::Option::None => ::core::option::Option::Some(self),
                    ::core::option::Option::Some((#krate::Segment::Field(key), rest)) => {
                        let _ = rest;
                        match key.as_str() {
                            #(#get_arms)*
                            _ => ::core::option::Option::None,
                        }
                    }
                    ::core::option::Option::Some(_) => ::core::option::Option::None,
                }
            }

            fn load(
                doc: &#krate::Doc,
                value: &#krate::__private::Value<'_>,
                id: &#krate::__private::ObjId,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                match value {
                    #krate::__private::Value::Object(#krate::__private::ObjType::Map) => {
                        <Self as #krate::Fields>::read_fields(doc, id)
                    }
                    _ => ::core::result::Result::Err(#krate::ModelError::shape("a map")),
                }
            }
            fn missing() -> ::core::result::Result<Self, #krate::ModelError> {
                ::core::result::Result::Ok(Self { #(#missing,)* })
            }
            fn store(
                &self,
                doc: &mut #krate::Doc,
                slot: #krate::Slot<'_>,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let obj = slot.object(doc, #krate::__private::ObjType::Map)?;
                <Self as #krate::Fields>::write_fields(self, doc, &obj)
            }
            fn refresh(
                &mut self,
                doc: &#krate::Doc,
                value: &#krate::__private::Value<'_>,
                id: &#krate::__private::ObjId,
                delta: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                match value {
                    #krate::__private::Value::Object(#krate::__private::ObjType::Map) => {
                        <Self as #krate::Fields>::refresh_fields(self, doc, id, delta)
                    }
                    _ => ::core::result::Result::Err(#krate::ModelError::shape("a map")),
                }
            }
            fn collect_text(&self, out: &mut ::std::vec::Vec<::std::string::String>) {
                let _ = &out;
                #(#texts)*
            }
        }

        impl #impl_generics #krate::Settable for #name #type_generics #where_clause {}

        impl #impl_generics #krate::Fields for #name #type_generics #where_clause {
            fn read_fields(
                doc: &#krate::Doc,
                obj: &#krate::__private::ObjId,
            ) -> ::core::result::Result<Self, #krate::ModelError> {
                let _ = (&doc, &obj);
                ::core::result::Result::Ok(Self { #(#reads,)* })
            }
            fn write_fields(
                &self,
                doc: &mut #krate::Doc,
                obj: &#krate::__private::ObjId,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let _ = (&doc, &obj);
                #(#writes)*
                ::core::result::Result::Ok(())
            }

            #[allow(unreachable_code, reason = "a record without fields has nothing to edit")]
            fn edit_fields(
                doc: &mut #krate::Doc,
                obj: &#krate::__private::ObjId,
                path: &[#krate::Segment],
                op: &#krate::EditOp,
                cx: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let _ = (&doc, &obj, &cx);
                match path.split_first() {
                    ::core::option::Option::Some((#krate::Segment::Field(key), rest)) => {
                        let _ = rest;
                        match key.as_str() {
                            #(#edit_arms)*
                            other => {
                                return ::core::result::Result::Err(#krate::ModelError::invalid(
                                    ::std::format!("no field `{other}`"),
                                ));
                            }
                        }
                    }
                    ::core::option::Option::None => match op {
                        #krate::EditOp::Set { value } => {
                            #whole_set
                        }
                        other => {
                            return ::core::result::Result::Err(#krate::ModelError::unsupported(other));
                        }
                    },
                    ::core::option::Option::Some((segment, _)) => {
                        return ::core::result::Result::Err(#krate::ModelError::invalid(
                            ::std::format!("nothing in this record is called `{segment}`"),
                        ));
                    }
                }
                #check_after_edit
                ::core::result::Result::Ok(())
            }

            #item_collections
            fn refresh_fields(
                &mut self,
                doc: &#krate::Doc,
                obj: &#krate::__private::ObjId,
                delta: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                let _ = (&doc, &obj, &delta);
                if delta.under(obj).is_all() {
                    *self = <Self as #krate::Fields>::read_fields(doc, obj)?;
                    return ::core::result::Result::Ok(());
                }
                #(#refreshes)*
                ::core::result::Result::Ok(())
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

        impl #impl_generics #name #type_generics #where_clause {
            /// The fields holding a `Collection`, by name: where actions on
            /// an item can act.
            #[doc(hidden)]
            pub const __COLLECTIONS: &'static [&'static str] = &[#(#collection_names),*];

            #[doc = #paths_fn_doc]
            #vis fn paths() -> #paths_name<Self, #(#generic_args),*> {
                <Self as #krate::Model>::paths_at(#krate::Path::root())
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

/// The declared type of a parameter of Rust type `ty`, and whether it is
/// optional. An app's own type is what its schema says it is written as
/// when `schema` (the type implements `JsonSchema`), else an object.
fn param_type(ty: &Type, schema: bool) -> (Tokens, bool) {
    let krate = krate();
    let pt = quote!(#krate::__private::ParamType);
    let (name, inner) = last_ident(ty).unwrap_or_default();
    match name.as_str() {
        "Option" => inner.map_or_else(
            || (quote!(#pt::Object), true),
            |inner| (param_type(inner, schema).0, true),
        ),
        "String" | "Id" | "Uuid" | "Atom" => (quote!(#pt::Text), false),
        "f32" | "f64" => (quote!(#pt::Number), false),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            (quote!(#pt::Integer), false)
        }
        "bool" => (quote!(#pt::Boolean), false),
        "Vec" => (quote!(#pt::Array), false),
        _ if !schema => (quote!(#pt::Object), false),
        _ => (
            quote!(#krate::__private::param_type_of(
                &<#ty as #krate::JsonSchema>::json_schema()
            )),
            false,
        ),
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
/// Action options: `label = "..."`, `destructive` (people confirm it, through
/// any door), `housekeeping` (a change about the record rather than to it,
/// such as a read marker), `external` (content from outside, nobody's undo
/// step), `permission = "..."`.
///
/// Resource options: `label`, `params = module` (the parameter module's
/// name), `initialize = path` (`fn() -> Self`, the deterministic genesis),
/// `seed = path` (`fn(&mut Self, &mut ActionCx)`, the creator's first
/// content), `available = path` (`fn(&Self, module::Verb) -> Availability`),
/// `children = path` (`fn(&Self) -> Vec<Derived<Self>>`), `search = path`,
/// `scopes = path`, `read_content = path` (`fn(&Self, Option<&Id>) ->
/// Result<serde_json::Value, String>`) and `views = path`.
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
