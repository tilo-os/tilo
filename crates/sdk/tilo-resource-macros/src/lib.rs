//! Procedural macros for `tilo-resource`. See that crate for the API; this
//! crate only generates calls into it.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, Fields, FnArg, GenericArgument, ImplItem,
    ItemImpl, Lit, Meta, MetaNameValue, Pat, PathArguments, Token, Type, parse_macro_input,
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
/// stored; `Default::default()` on read).
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
            } else {
                return Err(meta
                    .error("expected `skip`, `rename = \"...\"`, `label`, `readonly` or `unit`"));
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
            reads.push(quote!(#ident: ::core::default::Default::default()));
            missing.push(quote!(#ident: ::core::default::Default::default()));
            from_json.push(quote!(#ident: ::core::default::Default::default()));
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
        reads.push(quote!(#ident: #krate::read_key::<#ty>(doc, obj, #key)?));
        missing.push(quote!(
            #ident: <#ty as #krate::Model>::missing().map_err(|error| error.at(#key))?
        ));
        writes.push(quote!(#krate::write_key::<#ty>(doc, obj, #key, &self.#ident)?;));
        refreshes
            .push(quote!(#krate::refresh_key::<#ty>(&mut self.#ident, doc, obj, #key, delta)?;));
        texts.push(quote!(#krate::Model::collect_text(&self.#ident, out);));
        shapes.push(quote!(#krate::FieldShape {
            key: #key,
            name: #field_name,
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
        if last_ident(ty).is_some_and(|(name, _)| name == "Collection") {
            collections.push(quote!(#krate::items::ItemCollection {
                field: #field_name,
                items: self
                    .#ident
                    .iter()
                    .map(|(id, item)| {
                        (::core::clone::Clone::clone(id), #krate::__private::item_label(id, item))
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

struct ResourceOptions {
    kind: String,
    label: String,
    search: Option<syn::Path>,
    scopes: Option<syn::Path>,
    children: Option<syn::Path>,
}

fn resource_options(attr: TokenStream) -> syn::Result<ResourceOptions> {
    let metas = syn::parse::Parser::parse(Punctuated::<Meta, Token![,]>::parse_terminated, attr)?;
    let mut kind = None;
    let mut label = None;
    let mut search = None;
    let mut scopes = None;
    let mut children = None;
    for meta in metas {
        let Meta::NameValue(pair) = meta else {
            return Err(syn::Error::new(meta.span(), "expected `key = value`"));
        };
        let key = pair
            .path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        match (key.as_str(), &pair.value) {
            (
                "kind",
                Expr::Lit(ExprLit {
                    lit: Lit::Str(text),
                    ..
                }),
            ) => kind = Some(text.value()),
            (
                "label",
                Expr::Lit(ExprLit {
                    lit: Lit::Str(text),
                    ..
                }),
            ) => label = Some(text.value()),
            ("search", Expr::Path(path)) => search = Some(path.path.clone()),
            ("scopes", Expr::Path(path)) => scopes = Some(path.path.clone()),
            ("children", Expr::Path(path)) => children = Some(path.path.clone()),
            _ => {
                return Err(syn::Error::new(
                    pair.span(),
                    "expected `kind = \"...\"`, `label = \"...\"`, `search = path`, `scopes = path` or `children = path`",
                ));
            }
        }
    }
    let kind =
        kind.ok_or_else(|| syn::Error::new(Span::call_site(), "missing `kind = \"...\"`"))?;
    let label = label.unwrap_or_else(|| kind.clone());
    Ok(ResourceOptions {
        kind,
        label,
        search,
        scopes,
        children,
    })
}

struct ActionOptions {
    label: Option<String>,
    permission: Option<String>,
    destructive: bool,
    housekeeping: bool,
    external: bool,
    item: Option<String>,
}

fn action_options(attr: &Attribute) -> syn::Result<ActionOptions> {
    let mut options = ActionOptions {
        label: None,
        permission: None,
        destructive: false,
        housekeeping: false,
        external: false,
        item: None,
    };
    if matches!(attr.meta, Meta::Path(_)) {
        return Ok(options);
    }
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("label") {
            options.label = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("permission") {
            options.permission = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("destructive") {
            options.destructive = true;
        } else if meta.path.is_ident("housekeeping") {
            options.housekeeping = true;
        } else if meta.path.is_ident("external") {
            options.external = true;
        } else if meta.path.is_ident("item") {
            options.item = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else {
            return Err(meta.error(
                "expected `label`, `permission`, `destructive`, `housekeeping`, `external` or `item`",
            ));
        }
        if options.housekeeping && options.external {
            return Err(meta.error("an action is `housekeeping` or `external`, not both"));
        }
        Ok(())
    })?;
    Ok(options)
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

fn param_type(ty: &Type) -> (Tokens, bool) {
    let krate = krate();
    let pt = quote!(#krate::__private::ParamType);
    let (name, inner) = last_ident(ty).unwrap_or_default();
    match name.as_str() {
        "Option" => inner.map_or_else(
            || (quote!(#pt::Object), true),
            |inner| (param_type(inner).0, true),
        ),
        "String" | "Id" | "Uuid" | "Atom" => (quote!(#pt::Text), false),
        "f32" | "f64" => (quote!(#pt::Number), false),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            (quote!(#pt::Integer), false)
        }
        "bool" => (quote!(#pt::Boolean), false),
        "Vec" => (quote!(#pt::Array), false),
        _ => (quote!(#pt::Object), false),
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

/// Parameter guidance does not constrain the set of valid values.
struct ActionParamDescription {
    description: syn::LitStr,
    example: Option<syn::Expr>,
}

impl syn::parse::Parse for ActionParamDescription {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let description = input.parse()?;
        let example = if input.is_empty() {
            None
        } else {
            input.parse::<syn::Token![,]>()?;
            let name: syn::Ident = input.parse()?;
            if name != "example" {
                return Err(syn::Error::new(name.span(), "expected `example = ...`"));
            }
            input.parse::<syn::Token![=]>()?;
            Some(input.parse()?)
        };
        Ok(Self {
            description,
            example,
        })
    }
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
/// Generates `NoteAction` (one variant per action), `Note::EDIT` (an
/// `ActionRef` views bind controls to and tests apply) and `impl Resource`.
/// `#[param("Description", example = "value")]` publishes an example
/// callers can use without restricting other valid parameter values.
/// An action method takes `&mut self`, optionally `cx: &mut ActionCx`, then
/// its parameters, and returns `()` or `Result<(), impl Into<Refusal>>`.
/// An action whose first parameter is an item `Id` is also published on each
/// item of the collection it acts on, with the id bound (see
/// `tilo_resource::items`); `#[action(item = "field")]` names the collection
/// when its parameter name doesn't.
/// Options: `#[action(label = "...", permission = "comment", destructive)]`
/// (`destructive` applies both to the named declaration and to the same
/// operation reached through the generic `apply` action, so either path asks
/// for the same confirmation; `housekeeping` declares a change about the
/// record rather than to it, such as a read marker, which workspace activity
/// leaves out; `external` declares content that came from outside rather than
/// from anyone's edit, such as fetched rows, which syncs like any content but
/// is nobody's undo step);
/// `#[resource(search = path, scopes = path, children = path)]` override
/// search entries, the parts search can be narrowed to, and nested resources.
#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let options = match resource_options(attr) {
        Ok(options) => options,
        Err(error) => return error.to_compile_error().into(),
    };
    let mut item = parse_macro_input!(item as ItemImpl);
    match expand_resource(&options, &mut item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear expansion reads better than helpers threading a dozen token lists"
)]
fn expand_resource(options: &ResourceOptions, item: &mut ItemImpl) -> syn::Result<Tokens> {
    let krate = krate();
    let self_ty = item.self_ty.clone();
    let (type_name, _) = last_ident(&self_ty)
        .ok_or_else(|| syn::Error::new(self_ty.span(), "expected a named type"))?;
    let action_enum = format_ident!("{}Action", type_name);
    let kind = &options.kind;
    let label = &options.label;

    let mut variants = Vec::new();
    let mut arms = Vec::new();
    let mut risks = Vec::new();
    let mut permissions = Vec::new();
    // The least any action asks, ranked from none to administering.
    let mut least: Option<(u8, proc_macro2::TokenStream)> = None;
    let mut change_kinds = Vec::new();
    let mut consts = Vec::new();
    let mut decls = Vec::new();
    let mut item_actions = Vec::new();

    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let Some(position) = method
            .attrs
            .iter()
            .position(|attr| attr.path().is_ident("action"))
        else {
            continue;
        };
        let attr = method.attrs.remove(position);
        let action = action_options(&attr)?;
        let method_ident = method.sig.ident.clone();
        let method_name = method_ident.to_string();
        let id = method_name.replace('_', "-");
        let variant = format_ident!(
            "{}",
            method_name
                .split('_')
                .map(|word| {
                    let mut chars = word.chars();
                    chars.next().map_or_else(String::new, |first| {
                        first.to_uppercase().chain(chars).collect::<String>()
                    })
                })
                .collect::<String>()
        );
        let docs = doc_lines(&method.attrs);
        let label_text = action.label.clone().unwrap_or_else(|| {
            docs.first()
                .map(|line| line.trim_end_matches('.').to_owned())
                .filter(|line| !line.is_empty())
                .unwrap_or_else(|| humanize(&method_name))
        });
        let description = docs.join(" ");

        let mut takes_self = false;
        let mut takes_cx = false;
        let mut fields = Vec::new();
        for input in &mut method.sig.inputs {
            match input {
                FnArg::Receiver(receiver) => {
                    if receiver.mutability.is_none() || receiver.reference.is_none() {
                        return Err(syn::Error::new(
                            receiver.span(),
                            "an action takes `&mut self`",
                        ));
                    }
                    takes_self = true;
                }
                FnArg::Typed(typed) => {
                    if is_action_cx(&typed.ty) {
                        takes_cx = true;
                        continue;
                    }
                    let Pat::Ident(pat) = &*typed.pat else {
                        return Err(syn::Error::new(
                            typed.pat.span(),
                            "action parameters must be plain names",
                        ));
                    };
                    let mut described = None;
                    if let Some(index) = typed
                        .attrs
                        .iter()
                        .position(|attr| attr.path().is_ident("param"))
                    {
                        let attr = typed.attrs.remove(index);
                        let guidance = attr.parse_args::<ActionParamDescription>()?;
                        described = Some((guidance.description.value(), guidance.example));
                    }
                    let (owned, by_ref) = owned_type(&typed.ty);
                    fields.push((pat.ident.clone(), owned, described, by_ref));
                }
            }
        }
        if !takes_self {
            return Err(syn::Error::new(
                method.sig.span(),
                "an action takes `&mut self`",
            ));
        }
        // An action whose first parameter is an item id acts on an element
        // of a collection: it is also offered on each item, id bound.
        match fields.first() {
            Some((ident, ty, _, _)) if last_ident(ty).is_some_and(|(name, _)| name == "Id") => {
                let param = ident.to_string();
                let collection = action.item.as_ref().map_or_else(
                    || quote!(::core::option::Option::None),
                    |field| quote!(::core::option::Option::Some(#field)),
                );
                item_actions.push(quote!(#krate::items::ItemAction {
                    action: #id,
                    param: #param,
                    collection: #collection,
                }));
            }
            _ if action.item.is_some() => {
                return Err(syn::Error::new(
                    attr.span(),
                    "`item = \"...\"` needs the action's first parameter to be an item `Id`",
                ));
            }
            _ => {}
        }

        method.attrs.push(syn::parse_quote!(#[allow(
            clippy::needless_pass_by_value,
            clippy::missing_errors_doc,
            reason = "an action entry point: arguments arrive owned from the action payload and its docs are the action's description"
        )]));
        let field_idents: Vec<_> = fields
            .iter()
            .map(|(ident, _, _, _)| ident.clone())
            .collect();
        let call_args = fields.iter().map(|(ident, _, _, by_ref)| {
            if *by_ref {
                quote!(#ident)
            } else {
                quote!(::core::clone::Clone::clone(#ident))
            }
        });
        let field_defs = fields.iter().map(|(ident, ty, _, _)| {
            let optional = last_ident(ty).is_some_and(|(name, _)| name == "Option");
            if optional {
                quote!(#[serde(default)] #ident: #ty)
            } else {
                quote!(#ident: #ty)
            }
        });
        variants.push(quote! {
            #[serde(rename = #id)]
            #variant { #(#field_defs,)* }
        });
        let cx_arg = if takes_cx { quote!(cx,) } else { quote!() };
        arms.push(quote! {
            Self::#variant { #(#field_idents,)* } => #krate::Outcome::into_result(
                record.#method_ident(#cx_arg #(#call_args,)*)
            ),
        });
        if action.destructive {
            risks.push(quote! {
                Self::#variant { .. } => #krate::__private::RiskLevel::Destructive,
            });
        }
        if action.housekeeping {
            change_kinds.push(quote! {
                Self::#variant { .. } => #krate::__private::ChangeKind::Housekeeping,
            });
        }
        if action.external {
            change_kinds.push(quote! {
                Self::#variant { .. } => #krate::__private::ChangeKind::External,
            });
        }
        let const_ident = format_ident!("{}", method_name.to_uppercase());
        let param_names: Vec<String> = field_idents.iter().map(ToString::to_string).collect();
        let tys: Vec<&Type> = fields.iter().map(|(_, ty, _, _)| ty).collect();
        let input_ty = match tys.as_slice() {
            [] => quote!(()),
            [single] => quote!(#single),
            many => quote!((#(#many,)*)),
        };
        let const_doc = format!("Bind or apply the `{id}` action: {label_text}.");
        consts.push(quote! {
            #[doc = #const_doc]
            pub const #const_ident: #krate::ActionRef<#self_ty, #input_ty> =
                #krate::ActionRef::new(#id, &[#(#param_names),*]);
        });
        let params = fields.iter().map(|(ident, ty, described, _)| {
            let name = ident.to_string();
            let description = described.as_ref().map_or_else(|| humanize(&name), |(text, _)| text.clone());
            let (param_ty, optional) = param_type(ty);
            let parameter = if optional {
                quote!(#krate::__private::ActionParam::optional_owned(#name, #param_ty, #description))
            } else {
                quote!(#krate::__private::ActionParam::required_owned(#name, #param_ty, #description))
            };
            if let Some((_, Some(example))) = described {
                quote!(#parameter.with_schema(#krate::__private::serde_json::json!({"examples": [#example]})))
            } else {
                parameter
            }
        });
        let permission = match action.permission.as_deref() {
            None | Some("write") => quote!(Write),
            Some("none") => quote!(None),
            Some("read") => quote!(Read),
            Some("comment") => quote!(Comment),
            Some("invite") => quote!(Invite),
            Some("admin") => quote!(Admin),
            Some(other) => {
                return Err(syn::Error::new(
                    attr.span(),
                    format!("unknown permission `{other}`"),
                ));
            }
        };
        let destructive = if action.destructive {
            quote!(.destructive())
        } else {
            quote!()
        };
        let describe = if description.is_empty() {
            quote!()
        } else {
            quote!(.describe(#description))
        };
        let rank = match action.permission.as_deref() {
            Some("none") => 0,
            Some("read") => 1,
            Some("comment") => 2,
            None | Some("write") => 3,
            Some("invite") => 4,
            _ => 5,
        };
        if least.as_ref().is_none_or(|(least, _)| rank < *least) {
            least = Some((rank, permission.clone()));
        }
        permissions.push(quote! {
            #action_enum::#variant { .. } => #krate::__private::PermissionRequirement::#permission,
        });
        decls.push(quote! {
            #krate::ActionDecl::new(
                #id,
                #label_text,
                ::std::vec![#(#params),*],
                |args, now_ms| {
                    #krate::__private::decode_operation::<#action_enum>(#id, args).map(|operation| {
                        #krate::Stamped { operation, now_ms }
                    })
                },
            )
            #describe
            .requiring(#krate::__private::PermissionRequirement::#permission)
            #destructive
        });
    }

    let search = options.search.as_ref().map_or_else(
        || quote!(#krate::__private::default_search(#label, state)),
        |path| quote!(#path(state)),
    );
    let scopes = options.scopes.as_ref().map_or_else(
        || quote!(::std::vec::Vec::new()),
        |path| quote!(#path(state)),
    );
    let children = options.children.as_ref().map_or_else(
        || quote!(::std::vec::Vec::new()),
        |path| quote!(#path(state)),
    );
    let children = if item_actions.is_empty() {
        children
    } else {
        quote! {{
            let mut children: ::std::vec::Vec<#krate::__private::Resource> = #children;
            children.extend(#krate::items::item_children::<Self>(
                &<Self as #krate::Fields>::item_collections(state),
                &[#(#item_actions),*],
            ));
            children
        }}
    };
    let serde_path = format!("{CRATE}::__private::serde");
    let enum_doc =
        format!("The replayable actions of [`{type_name}`], one per `#[action]` method.");
    let cx_binding = if arms.is_empty() {
        quote!(_cx)
    } else {
        quote!(cx)
    };
    let record_binding = if arms.is_empty() {
        quote!(_record)
    } else {
        quote!(record)
    };
    let risk = if risks.is_empty() {
        quote!()
    } else {
        quote! {
            fn risk(&self) -> #krate::__private::RiskLevel {
                #[allow(unreachable_patterns, reason = "every other operation is low risk")]
                match self {
                    #(#risks)*
                    _ => #krate::__private::RiskLevel::Low,
                }
            }
        }
    };
    let change_kind = if change_kinds.is_empty() {
        quote!()
    } else {
        quote! {
            fn change_kind(&self) -> #krate::__private::ChangeKind {
                #[allow(unreachable_patterns, reason = "every other operation changes content")]
                match self {
                    #(#change_kinds)*
                    _ => #krate::__private::ChangeKind::Content,
                }
            }
        }
    };
    // An action declared at another level than writing is applied at that
    // level too, whichever door it comes through.
    let least_fn = match least {
        Some((rank, least)) if rank != 3 => quote! {
            fn least_permission() -> #krate::__private::PermissionRequirement {
                #krate::__private::PermissionRequirement::#least
            }
        },
        _ => quote!(),
    };
    let permission_fn = if permissions.is_empty() {
        quote!()
    } else {
        quote! {
            fn permission(action: &Self::Action) -> #krate::__private::PermissionRequirement {
                match &action.operation {
                    #(#permissions)*
                    #action_enum::FieldEdit(_) => #krate::__private::PermissionRequirement::Write,
                }
            }

            #least_fn
        }
    };
    let body = quote! {
        match self {
            #(#arms)*
            Self::FieldEdit(_) => ::core::result::Result::Err(#krate::Refusal::new(
                "a field edit applies at its path",
            )),
        }
    };

    Ok(quote! {
        #item

        #[doc = #enum_doc]
        #[derive(Clone, Debug, #krate::__private::serde::Serialize, #krate::__private::serde::Deserialize)]
        #[serde(crate = #serde_path, tag = "action", content = "args")]
        pub enum #action_enum {
            #(#variants,)*
            /// A field edit: a path into the record and an operation.
            #[serde(rename = "field-edit")]
            FieldEdit(#krate::Edit<#self_ty>),
        }

        impl #krate::Operation for #action_enum {
            type Record = #self_ty;
            fn edit(&self) -> ::core::option::Option<&#krate::Edit<#self_ty>> {
                match self {
                    Self::FieldEdit(edit) => ::core::option::Option::Some(edit),
                    #[allow(unreachable_patterns, reason = "a record may have no verbs")]
                    _ => ::core::option::Option::None,
                }
            }
            #[allow(unused_variables, reason = "a method may ignore the action context")]
            fn perform(
                &self,
                #record_binding: &mut #self_ty,
                #cx_binding: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::Refusal> {
                #body
            }
            #risk
            #change_kind
        }

        impl #self_ty {
            #(#consts)*
        }

        impl #krate::Resource for #self_ty {
            const KIND: &'static str = #kind;
            const LABEL: &'static str = #label;
            type State = #self_ty;
            type Action = #krate::Stamped<#action_enum>;

            fn read(doc: &#krate::Doc) -> ::core::result::Result<Self, #krate::ModelError> {
                <Self as #krate::Fields>::read_fields(doc, &#krate::__private::ROOT)
            }

            fn refresh(
                state: &mut Self,
                doc: &#krate::Doc,
                delta: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                <Self as #krate::Fields>::refresh_fields(state, doc, &#krate::__private::ROOT, delta)
            }

            fn initialize(doc: &mut #krate::Doc) -> ::core::result::Result<(), #krate::ModelError> {
                <Self as #krate::Fields>::write_fields(
                    &<Self as ::core::default::Default>::default(),
                    doc,
                    &#krate::__private::ROOT,
                )
            }

            fn declarations() -> ::std::vec::Vec<#krate::ActionDecl<Self>> {
                ::std::vec![#(#decls),*]
            }

            const EDITS: bool = true;

            fn edit_action(edit: #krate::Edit<Self>) -> ::core::option::Option<Self::Action> {
                ::core::option::Option::Some(#krate::Stamped {
                    operation: #action_enum::FieldEdit(edit),
                    now_ms: 0,
                })
            }

            fn stamp(action: &mut Self::Action, now_ms: i64) {
                action.now_ms = now_ms;
            }

            fn kind_of(action: &Self::Action) -> #krate::ActionKind {
                #krate::Operation::kind(&action.operation)
            }

            fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
                match (&previous.operation, &next.operation) {
                    (#action_enum::FieldEdit(previous), #action_enum::FieldEdit(next)) => {
                        previous.coalesces(next)
                    }
                    _ => false,
                }
            }

            #permission_fn

            fn children(state: &Self) -> ::std::vec::Vec<#krate::__private::Resource> {
                let _ = state;
                #children
            }

            fn search(state: &Self) -> ::std::vec::Vec<#krate::SearchEntry> {
                #search
            }

            fn search_scopes(state: &Self) -> ::std::vec::Vec<#krate::ScopeDecl> {
                let _ = state;
                #scopes
            }

            fn shape() -> ::core::option::Option<#krate::Shape> {
                ::core::option::Option::Some(<Self as #krate::Model>::SHAPE)
            }

            fn action_schema() -> ::core::option::Option<#krate::__private::serde_json::Value> {
                ::core::option::Option::Some(#krate::schema::record_action_schema::<Self>())
            }
        }
    })
}
