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
#[proc_macro_derive(JsonSchema, attributes(serde))]
pub fn derive_json_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match schema::derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

mod schema;

#[derive(Default)]
struct FieldOptions {
    rename: Option<String>,
    skip: bool,
}

fn field_options(attrs: &[Attribute]) -> syn::Result<FieldOptions> {
    let mut options = FieldOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("model")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                options.skip = true;
                Ok(())
            } else if meta.path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                options.rename = Some(value.value());
                Ok(())
            } else {
                Err(meta.error("expected `skip` or `rename = \"...\"`"))
            }
        })?;
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
            Fields::Named(fields) => {
                let mut reads = Vec::new();
                let mut missing = Vec::new();
                let mut writes = Vec::new();
                let mut refreshes = Vec::new();
                let mut texts = Vec::new();
                let mut shapes = Vec::new();
                let mut collections = Vec::new();
                for field in &fields.named {
                    let ident = field.ident.as_ref().ok_or_else(|| {
                        syn::Error::new(field.span(), "named field without a name")
                    })?;
                    let ty = &field.ty;
                    let options = field_options(&field.attrs)?;
                    let key = options.rename.unwrap_or_else(|| ident.to_string());
                    if options.skip {
                        reads.push(quote!(#ident: ::core::default::Default::default()));
                        missing.push(quote!(#ident: ::core::default::Default::default()));
                        continue;
                    }
                    reads.push(quote!(#ident: #krate::read_key::<#ty>(doc, obj, #key)?));
                    missing.push(quote!(
                        #ident: <#ty as #krate::Model>::missing().map_err(|error| error.at(#key))?
                    ));
                    writes.push(quote!(#krate::write_key::<#ty>(doc, obj, #key, &self.#ident)?;));
                    refreshes.push(
                        quote!(#krate::refresh_key::<#ty>(&mut self.#ident, doc, obj, #key, delta)?;),
                    );
                    texts.push(quote!(#krate::Model::collect_text(&self.#ident, out);));
                    shapes.push(quote!(#krate::FieldShape {
                        key: #key,
                        shape: <#ty as #krate::Model>::SHAPE,
                    }));
                    if last_ident(ty).is_some_and(|(name, _)| name == "Collection") {
                        let field_name = ident.to_string();
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
                Ok(quote! {
                    impl #impl_generics #krate::Model for #name #type_generics #where_clause {
                        const SHAPE: #krate::Shape = #krate::Shape::Record(&[#(#shapes),*]);

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
                })
            }
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                let ty = fields
                    .unnamed
                    .first()
                    .map(|field| &field.ty)
                    .ok_or_else(|| syn::Error::new(fields.span(), "expected one field"))?;
                Ok(quote! {
                    impl #impl_generics #krate::Model for #name #type_generics #where_clause {
                        const SHAPE: #krate::Shape = <#ty as #krate::Model>::SHAPE;

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
                impl #impl_generics #krate::Model for #name #type_generics #where_clause {
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
    children: Option<syn::Path>,
}

fn resource_options(attr: TokenStream) -> syn::Result<ResourceOptions> {
    let metas = syn::parse::Parser::parse(Punctuated::<Meta, Token![,]>::parse_terminated, attr)?;
    let mut kind = None;
    let mut label = None;
    let mut search = None;
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
            ("children", Expr::Path(path)) => children = Some(path.path.clone()),
            _ => {
                return Err(syn::Error::new(
                    pair.span(),
                    "expected `kind = \"...\"`, `label = \"...\"`, `search = path` or `children = path`",
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
/// `#[resource(search = path, children = path)]` override search entries and
/// nested resources.
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
                        described = Some(attr.parse_args::<syn::LitStr>()?.value());
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
            let description = described.clone().unwrap_or_else(|| humanize(&name));
            let (param_ty, optional) = param_type(ty);
            if optional {
                quote!(#krate::__private::ActionParam::optional_owned(#name, #param_ty, #description))
            } else {
                quote!(#krate::__private::ActionParam::required_owned(#name, #param_ty, #description))
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
    let body = if arms.is_empty() {
        quote!(match *self {})
    } else {
        quote!(match self { #(#arms)* })
    };

    Ok(quote! {
        #item

        #[doc = #enum_doc]
        #[derive(Clone, Debug, #krate::__private::serde::Serialize, #krate::__private::serde::Deserialize)]
        #[serde(crate = #serde_path, tag = "action", content = "args")]
        pub enum #action_enum {
            #(#variants,)*
        }

        impl #krate::Operation for #action_enum {
            type Record = #self_ty;
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

            fn children(state: &Self) -> ::std::vec::Vec<#krate::__private::Resource> {
                let _ = state;
                #children
            }

            fn search(state: &Self) -> ::std::vec::Vec<#krate::SearchEntry> {
                #search
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
