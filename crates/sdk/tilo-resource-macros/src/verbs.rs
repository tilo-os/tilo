//! `#[resource]`: a record's verbs, their parameter structs and the
//! resource implementation.

use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, FnArg, ImplItem, ItemImpl, Lit, Meta, Pat, Token, Type};

use crate::{
    CRATE, doc_lines, humanize, is_action_cx, krate, last_ident, owned_type, param_type, snake_case,
};

pub(crate) struct ResourceOptions {
    kind: String,
    label: String,
    params: Option<syn::Ident>,
    search: Option<syn::Path>,
    scopes: Option<syn::Path>,
    children: Option<syn::Path>,
    initialize: Option<syn::Path>,
    seed: Option<syn::Path>,
    available: Option<syn::Path>,
    read_content: Option<syn::Path>,
    views: Option<syn::Path>,
    version: Option<syn::LitInt>,
    migrate: Option<syn::Path>,
}

const RESOURCE_KEYS: &str = "expected `kind = \"...\"`, `label = \"...\"`, `params = name`, `search = path`, \
     `scopes = path`, `children = path`, `initialize = path`, `seed = path`, `available = path`, \
     `read_content = path`, `views = path`, `version = N` or `migrate = path`";

pub(crate) fn resource_options(attr: proc_macro::TokenStream) -> syn::Result<ResourceOptions> {
    let metas = syn::parse::Parser::parse(Punctuated::<Meta, Token![,]>::parse_terminated, attr)?;
    let mut options = ResourceOptions {
        kind: String::new(),
        label: String::new(),
        params: None,
        search: None,
        scopes: None,
        children: None,
        initialize: None,
        seed: None,
        available: None,
        read_content: None,
        views: None,
        version: None,
        migrate: None,
    };
    let mut kind = None;
    let mut label = None;
    for meta in metas {
        let Meta::NameValue(pair) = meta else {
            return Err(syn::Error::new(meta.span(), RESOURCE_KEYS));
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
            ("params", Expr::Path(path)) if path.path.get_ident().is_some() => {
                options.params = path.path.get_ident().cloned();
            }
            ("search", Expr::Path(path)) => options.search = Some(path.path.clone()),
            ("scopes", Expr::Path(path)) => options.scopes = Some(path.path.clone()),
            ("children", Expr::Path(path)) => options.children = Some(path.path.clone()),
            ("initialize", Expr::Path(path)) => options.initialize = Some(path.path.clone()),
            ("seed", Expr::Path(path)) => options.seed = Some(path.path.clone()),
            ("available", Expr::Path(path)) => options.available = Some(path.path.clone()),
            ("read_content", Expr::Path(path)) => options.read_content = Some(path.path.clone()),
            ("views", Expr::Path(path)) => options.views = Some(path.path.clone()),
            (
                "version",
                Expr::Lit(ExprLit {
                    lit: Lit::Int(version),
                    ..
                }),
            ) => options.version = Some(version.clone()),
            ("migrate", Expr::Path(path)) => options.migrate = Some(path.path.clone()),
            _ => return Err(syn::Error::new(pair.span(), RESOURCE_KEYS)),
        }
    }
    options.kind =
        kind.ok_or_else(|| syn::Error::new(Span::call_site(), "missing `kind = \"...\"`"))?;
    if options.version.is_some() != options.migrate.is_some() {
        return Err(syn::Error::new(
            Span::call_site(),
            "`version = N` and `migrate = path` go together: a new version needs a migration",
        ));
    }
    options.label = label.unwrap_or_else(|| options.kind.clone());
    Ok(options)
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent action attribute"
)]
struct ActionOptions {
    id: Option<String>,
    label: Option<String>,
    permission: Option<String>,
    destructive: bool,
    housekeeping: bool,
    external: bool,
    item: Option<syn::Ident>,
}

fn action_options(attr: &Attribute) -> syn::Result<ActionOptions> {
    let mut options = ActionOptions {
        id: None,
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
        if meta.path.is_ident("id") {
            options.id = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("label") {
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
            options.item = Some(meta.value()?.parse::<syn::Ident>()?);
        } else {
            return Err(meta.error(
                "expected `id = \"...\"`, `label`, `permission`, `destructive`, `housekeeping`, `external` or `item = field`",
            ));
        }
        if options.housekeeping && options.external {
            return Err(meta.error("an action is `housekeeping` or `external`, not both"));
        }
        Ok(())
    })?;
    Ok(options)
}

/// `#[param("description", name = "public", example = expr)]`, each part
/// optional.
#[derive(Default)]
struct ParamOptions {
    description: Option<String>,
    name: Option<String>,
    example: Option<Expr>,
}

impl syn::parse::Parse for ParamOptions {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut options = Self::default();
        if input.peek(syn::LitStr) {
            options.description = Some(input.parse::<syn::LitStr>()?.value());
            if input.is_empty() {
                return Ok(options);
            }
            input.parse::<Token![,]>()?;
        }
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            if key == "name" {
                options.name = Some(input.parse::<syn::LitStr>()?.value());
            } else if key == "example" {
                options.example = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `\"description\"`, `name = \"...\"` or `example = ...`",
                ));
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(options)
    }
}

struct Param {
    ident: syn::Ident,
    public: String,
    ty: Type,
    by_ref: bool,
    description: String,
    example: Option<Expr>,
}

enum Input {
    Cx,
    /// An outbox of the record, by its field.
    Outbox(syn::Ident),
    Param(usize),
}

fn is_outbox(ty: &Type) -> bool {
    let Type::Reference(reference) = ty else {
        return false;
    };
    reference.mutability.is_some()
        && last_ident(&reference.elem).is_some_and(|(name, _)| name == "Outbox")
}

fn pascal_case(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect::<String>()
            })
        })
        .collect()
}

/// One verb as `#[resource]` reads it.
struct Verb {
    method: syn::Ident,
    id: String,
    variant: syn::Ident,
    label: String,
    description: String,
    options: ActionOptions,
    inputs: Vec<Input>,
    params: Vec<Param>,
}

fn read_verb(method: &mut syn::ImplItemFn, attr: &Attribute) -> syn::Result<Verb> {
    let options = action_options(attr)?;
    let method_ident = method.sig.ident.clone();
    let method_name = method_ident.to_string();
    let id = options
        .id
        .clone()
        .unwrap_or_else(|| method_name.replace('_', "-"));
    let variant = format_ident!("{}", pascal_case(&method_name));
    let docs = doc_lines(&method.attrs);
    let label = options.label.clone().unwrap_or_else(|| {
        docs.first()
            .map(|line| line.trim_end_matches('.').to_owned())
            .filter(|line| !line.is_empty())
            .unwrap_or_else(|| humanize(&method_name))
    });
    let description = docs.join(" ");
    let mut takes_self = false;
    let mut inputs = Vec::new();
    let mut params = Vec::new();
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
                let Pat::Ident(pat) = &*typed.pat else {
                    return Err(syn::Error::new(
                        typed.pat.span(),
                        "action parameters must be plain names",
                    ));
                };
                if is_action_cx(&typed.ty) {
                    inputs.push(Input::Cx);
                    continue;
                }
                if is_outbox(&typed.ty) {
                    inputs.push(Input::Outbox(pat.ident.clone()));
                    continue;
                }
                let mut param_options = ParamOptions::default();
                if let Some(index) = typed
                    .attrs
                    .iter()
                    .position(|attr| attr.path().is_ident("param"))
                {
                    let attr = typed.attrs.remove(index);
                    param_options = attr.parse_args::<ParamOptions>()?;
                }
                let (owned, by_ref) = owned_type(&typed.ty);
                let name = pat.ident.to_string();
                inputs.push(Input::Param(params.len()));
                params.push(Param {
                    ident: pat.ident.clone(),
                    public: param_options.name.unwrap_or_else(|| name.clone()),
                    ty: owned,
                    by_ref,
                    description: param_options.description.unwrap_or_else(|| humanize(&name)),
                    example: param_options.example,
                });
            }
        }
    }
    if !takes_self {
        return Err(syn::Error::new(
            method.sig.span(),
            "an action takes `&mut self`",
        ));
    }
    Ok(Verb {
        method: method_ident,
        id,
        variant,
        label,
        description,
        options,
        inputs,
        params,
    })
}

/// The parameter struct of one verb, its schema, conversions and paths.
#[allow(
    clippy::too_many_lines,
    reason = "one linear expansion reads better than helpers threading a dozen token lists"
)]
fn params_struct(verb: &Verb, self_ty: &Type, type_name: &str) -> Tokens {
    let krate = krate();
    let name = &verb.variant;
    let paths_name = format_ident!("{}Paths", name);
    let serde_path = format!("{CRATE}::__private::serde");
    let const_ident = format_ident!("{}", verb.method.to_string().to_uppercase());
    let doc = format!(
        "The parameters of [`{type_name}::{}`]: {}.",
        verb.method, verb.label
    );
    let fields = verb.params.iter().map(|param| {
        let ident = &param.ident;
        let ty = &param.ty;
        let description = &param.description;
        let rename = (*ident != param.public).then(|| {
            let public = &param.public;
            quote!(#[serde(rename = #public)])
        });
        let default = last_ident(ty)
            .is_some_and(|(name, _)| name == "Option")
            .then(|| quote!(#[serde(default, skip_serializing_if = "Option::is_none")]));
        quote! {
            #[doc = #description]
            #rename
            #default
            pub #ident: #ty
        }
    });
    let schemas = verb.params.iter().map(|param| {
        let ty = &param.ty;
        let public = &param.public;
        let description = &param.description;
        let example = param.example.as_ref().map_or_else(
            || quote!(::core::option::Option::None),
            |example| {
                quote!(::core::option::Option::Some(
                    #krate::__private::serde_json::json!(#example)
                ))
            },
        );
        quote! {(
            #public,
            #krate::__private::param_schema(
                <#ty as #krate::JsonSchema>::json_schema(),
                #description,
                #example,
            ),
            !<#ty as #krate::JsonSchema>::OPTIONAL,
        )}
    });
    let path_fields = verb.params.iter().map(|param| {
        let ident = &param.ident;
        let ty = &param.ty;
        quote!(pub #ident: #krate::Path<#name, #ty>)
    });
    let path_inits = verb.params.iter().map(|param| {
        let ident = &param.ident;
        let public = &param.public;
        quote!(#ident: #krate::Path::<#name, #name>::root().child(
            #krate::Segment::Field(::std::string::String::from(#public))
        ))
    });
    let idents: Vec<&syn::Ident> = verb.params.iter().map(|param| &param.ident).collect();
    let tys: Vec<&Type> = verb.params.iter().map(|param| &param.ty).collect();
    let conversions = match (idents.as_slice(), tys.as_slice()) {
        ([], []) => quote! {
            impl ::core::convert::From<()> for #name {
                fn from((): ()) -> Self {
                    Self {}
                }
            }
            impl #krate::NoParams for #name {}
        },
        ([ident], [ty]) => {
            let text = last_ident(ty)
                .is_some_and(|(name, _)| name == "String")
                .then(|| {
                    quote! {
                        impl ::core::convert::From<&str> for #name {
                            fn from(value: &str) -> Self {
                                Self { #ident: ::std::string::String::from(value) }
                            }
                        }
                    }
                });
            quote! {
                impl ::core::convert::From<#ty> for #name {
                    fn from(#ident: #ty) -> Self {
                        Self { #ident }
                    }
                }
                #text
            }
        }
        (idents, tys) => quote! {
            impl ::core::convert::From<(#(#tys,)*)> for #name {
                fn from((#(#idents,)*): (#(#tys,)*)) -> Self {
                    Self { #(#idents,)* }
                }
            }
        },
    };
    let derive_default = verb
        .params
        .is_empty()
        .then(|| quote!(::core::default::Default,));
    let label = &verb.label;
    quote! {
        #[doc = #doc]
        #[derive(Clone, Debug, #derive_default #krate::__private::serde::Serialize, #krate::__private::serde::Deserialize)]
        #[serde(crate = #serde_path)]
        pub struct #name {
            #(#fields,)*
        }

        impl #krate::JsonSchema for #name {
            fn json_schema() -> #krate::__private::serde_json::Value {
                #krate::schema::describe(
                    #krate::schema::object(::std::vec![#(#schemas),*]),
                    #label,
                )
            }
        }

        #conversions

        impl #krate::Params for #name {
            type Resource = #self_ty;
            const ACTION: #krate::ActionRef<#self_ty, Self> = <#self_ty>::#const_ident;
        }

        /// The typed paths into the parameters, for a form that edits them.
        #[derive(Clone, Debug)]
        pub struct #paths_name {
            #(#path_fields,)*
        }

        impl #name {
            /// The typed paths into the parameters.
            #[must_use]
            pub fn paths() -> #paths_name {
                #paths_name { #(#path_inits,)* }
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear expansion reads better than helpers threading a dozen token lists"
)]
pub(crate) fn expand(options: &ResourceOptions, item: &mut ItemImpl) -> syn::Result<Tokens> {
    let krate = krate();
    let self_ty = item.self_ty.clone();
    let (type_name, _) = last_ident(&self_ty)
        .ok_or_else(|| syn::Error::new(self_ty.span(), "expected a named type"))?;
    let action_enum = format_ident!("{}Action", type_name);
    let params_mod = options
        .params
        .clone()
        .unwrap_or_else(|| format_ident!("{}", snake_case(&type_name)));
    let kind = &options.kind;
    let label = &options.label;

    let mut verbs = Vec::new();
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
        let verb = read_verb(method, &attr)?;
        method.attrs.push(syn::parse_quote!(#[allow(
            clippy::needless_pass_by_value,
            clippy::missing_errors_doc,
            reason = "an action entry point: arguments arrive owned from the action payload and its docs are the action's description"
        )]));
        let takes_item = verb
            .params
            .first()
            .is_some_and(|param| last_ident(&param.ty).is_some_and(|(name, _)| name == "Id"));
        if verb.options.item.is_some() && !takes_item {
            return Err(syn::Error::new(
                attr.span(),
                "`item = field` needs the action's first parameter to be an item `Id`",
            ));
        }
        verbs.push(verb);
    }
    let mut seen = std::collections::BTreeSet::new();
    for verb in &verbs {
        if !seen.insert(verb.id.clone()) {
            return Err(syn::Error::new(
                verb.method.span(),
                format!("two actions are published as `{}`", verb.id),
            ));
        }
    }

    let mut structs = Vec::new();
    let mut variants = Vec::new();
    let mut arms = Vec::new();
    let mut risks = Vec::new();
    let mut kinds = Vec::new();
    let mut permissions = Vec::new();
    let mut least: Option<(u8, Tokens)> = None;
    let mut change_kinds = Vec::new();
    let mut consts = Vec::new();
    let mut decls = Vec::new();
    let mut item_actions = Vec::new();
    let mut item_checks = Vec::new();
    let mut verb_variants = Vec::new();
    let mut verb_ids = Vec::new();

    for verb in &verbs {
        let id = &verb.id;
        let variant = &verb.variant;
        let method = &verb.method;
        let label_text = &verb.label;
        structs.push(params_struct(verb, &self_ty, &type_name));
        variants.push(quote! {
            #[serde(rename = #id)]
            #variant(#params_mod::#variant)
        });
        verb_variants.push(variant.clone());
        verb_ids.push(id.clone());

        let outboxes: Vec<&syn::Ident> = verb
            .inputs
            .iter()
            .filter_map(|input| match input {
                Input::Outbox(field) => Some(field),
                _ => None,
            })
            .collect();
        let taken: Vec<syn::Ident> = outboxes
            .iter()
            .map(|field| format_ident!("__outbox_{}", field))
            .collect();
        let call_args = verb.inputs.iter().map(|input| match input {
            Input::Cx => quote!(cx),
            Input::Outbox(field) => {
                let taken = format_ident!("__outbox_{}", field);
                quote!(&mut #taken)
            }
            Input::Param(index) => {
                let Some(param) = verb.params.get(*index) else {
                    return quote!();
                };
                let ident = &param.ident;
                if param.by_ref {
                    quote!(&params.#ident)
                } else {
                    quote!(::core::clone::Clone::clone(&params.#ident))
                }
            }
        });
        arms.push(quote! {
            Self::#variant(params) => {
                let _ = &params;
                #(let mut #taken = ::core::mem::take(&mut record.#outboxes);)*
                let outcome = #krate::Outcome::into_result(record.#method(#(#call_args),*));
                #(record.#outboxes = #taken;)*
                outcome
            }
        });
        let action_kind = if outboxes.is_empty() {
            quote!(Undoable)
        } else {
            quote!(Outside)
        };
        kinds.push(quote!(Self::#variant(..) => #krate::ActionKind::#action_kind,));
        if verb.options.destructive {
            risks.push(quote!(Self::#variant(..) => #krate::__private::RiskLevel::Destructive,));
        }
        if verb.options.housekeeping {
            change_kinds
                .push(quote!(Self::#variant(..) => #krate::__private::ChangeKind::Housekeeping,));
        }
        if verb.options.external {
            change_kinds
                .push(quote!(Self::#variant(..) => #krate::__private::ChangeKind::External,));
        }

        let const_ident = format_ident!("{}", method.to_string().to_uppercase());
        let const_doc = format!("Bind or apply the `{id}` action: {label_text}.");
        consts.push(quote! {
            #[doc = #const_doc]
            pub const #const_ident: #krate::ActionRef<#self_ty, #params_mod::#variant> =
                #krate::ActionRef::new(#id, &[]);
        });

        // An action whose first parameter is an item id acts on an element
        // of a collection: it is also offered on each item, id bound.
        if let Some(first) = verb.params.first()
            && last_ident(&first.ty).is_some_and(|(name, _)| name == "Id")
        {
            let param = &first.public;
            let named = verb
                .options
                .item
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            let collection = if named.is_empty() {
                quote!(::core::option::Option::None)
            } else {
                quote!(::core::option::Option::Some(#named))
            };
            item_actions.push(quote!(#krate::items::ItemAction {
                action: #id,
                param: #param,
                collection: #collection,
            }));
            let message = if named.is_empty() {
                format!(
                    "`{}` acts on an item, but no collection of `{type_name}` is named `{}` and it has more than one: name it with `#[action(item = field)]`",
                    verb.method, first.public
                )
            } else {
                format!("`{type_name}` has no collection named `{named}`")
            };
            item_checks.push(quote! {
                const _: () = ::core::assert!(
                    #krate::items::item_target(<#self_ty>::__COLLECTIONS, #param, #named),
                    #message,
                );
            });
        }

        let params = verb.params.iter().map(|param| {
            let name = &param.public;
            let ty = &param.ty;
            let description = &param.description;
            let (param_ty, optional) = param_type(ty);
            let example = param.example.as_ref().map_or_else(
                || quote!(::core::option::Option::None),
                |example| {
                    quote!(::core::option::Option::Some(
                        #krate::__private::serde_json::json!(#example)
                    ))
                },
            );
            let constructor = if optional {
                quote!(optional_owned)
            } else {
                quote!(required_owned)
            };
            quote! {
                #krate::__private::ActionParam::#constructor(#name, #param_ty, #description)
                    .with_schema(#krate::__private::param_schema(
                        <#ty as #krate::JsonSchema>::json_schema(),
                        #description,
                        #example,
                    ))
            }
        });
        let permission = match verb.options.permission.as_deref() {
            None | Some("write") => quote!(Write),
            Some("none") => quote!(None),
            Some("read") => quote!(Read),
            Some("comment") => quote!(Comment),
            Some("invite") => quote!(Invite),
            Some("admin") => quote!(Admin),
            Some(other) => {
                return Err(syn::Error::new(
                    method.span(),
                    format!("unknown permission `{other}`"),
                ));
            }
        };
        let destructive = verb.options.destructive.then(|| quote!(.destructive()));
        let describe = (!verb.description.is_empty()).then(|| {
            let description = &verb.description;
            quote!(.describe(#description))
        });
        let rank = match verb.options.permission.as_deref() {
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
            #action_enum::#variant(..) => #krate::__private::PermissionRequirement::#permission,
        });
        decls.push(quote! {
            #krate::ActionDecl::new(
                #id,
                #label_text,
                ::std::vec![#(#params),*],
                |args, now_ms| {
                    let params: #params_mod::#variant = #krate::__private::serde_json::from_value(
                        #krate::__private::serde_json::Value::Object(args.to_json()),
                    )
                    .map_err(|error| ::std::string::ToString::to_string(&error))?;
                    ::core::result::Result::Ok(#krate::Stamped {
                        operation: #action_enum::#variant(params),
                        now_ms,
                    })
                },
            )
            #describe
            .requiring(#krate::__private::PermissionRequirement::#permission)
            .of_kind(#krate::ActionKind::#action_kind)
            #destructive
        });
    }

    // Generated hooks.
    let search = options.search.as_ref().map_or_else(
        || quote!(#krate::__private::default_search::<Self>(#label, state)),
        |path| quote!(#path(state)),
    );
    let scopes = options.scopes.as_ref().map_or_else(
        || quote!(::std::vec::Vec::new()),
        |path| quote!(#path(state)),
    );
    let derived = options.children.as_ref().map_or_else(
        || quote!(::std::vec::Vec::new()),
        |path| {
            quote! {
                #path(state)
                    .iter()
                    .map(#krate::Derived::<Self>::publish)
                    .collect::<::std::vec::Vec<_>>()
            }
        },
    );
    let derived_child = options.children.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                if let ::core::option::Option::Some(found) = #path(state)
                    .iter()
                    .map(#krate::Derived::<Self>::publish)
                    .find(|child| child.id.as_str() == id)
                {
                    return ::core::option::Option::Some(found);
                }
            }
        },
    );
    let item_child = if item_actions.is_empty() {
        quote!(::core::option::Option::None)
    } else {
        quote! {
            #krate::items::item_child::<Self>(
                id,
                <Self>::__COLLECTIONS,
                &[#(#item_actions),*],
                |field, item| <Self as #krate::Fields>::item_label(state, field, item),
            )
        }
    };
    let children = if item_actions.is_empty() {
        derived
    } else {
        quote! {{
            let mut children: ::std::vec::Vec<#krate::__private::Resource> = #derived;
            children.extend(#krate::items::item_children::<Self>(
                &<Self as #krate::Fields>::item_collections(state),
                &[#(#item_actions),*],
            ));
            children
        }}
    };
    let initial = options.initialize.as_ref().map_or_else(
        || quote!(<Self as ::core::default::Default>::default()),
        |path| quote!(#path()),
    );
    let (seed_variant, seed_arm, seed_fn, seed_change_kind) = options.seed.as_ref().map_or_else(
        || (quote!(), quote!(), quote!(), quote!()),
        |path| {
            (
                quote! {
                    /// The record's first content, written by its creator.
                    #[serde(rename = "seed")]
                    Seed,
                },
                quote! {
                    Self::Seed => #krate::Outcome::into_result(#path(record, cx)),
                },
                quote! {
                    fn seed() -> ::core::option::Option<Self::Action> {
                        ::core::option::Option::Some(#krate::Stamped {
                            operation: #action_enum::Seed,
                            now_ms: 0,
                        })
                    }
                },
                quote!(Self::Seed => #krate::__private::ChangeKind::External,),
            )
        },
    );
    if options.seed.is_some() {
        change_kinds.push(seed_change_kind);
    }
    let availability = options.available.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                fn availability(state: &Self, action: &str) -> #krate::Availability {
                    match action {
                        #(#verb_ids => #path(state, #params_mod::Verb::#verb_variants),)*
                        _ => #krate::Availability::Available,
                    }
                }
            }
        },
    );
    let version = options.version.as_ref().map_or_else(
        || quote!(),
        |version| {
            let migrate = &options.migrate;
            quote! {
                const VERSION: u32 = #version;

                fn migrate(
                    doc: &mut #krate::Doc,
                    from: u32,
                ) -> ::core::result::Result<(), #krate::ModelError> {
                    #migrate(doc, from)
                }
            }
        },
    );
    let read_content = options.read_content.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                const READABLE: bool = true;
                fn read_content(
                    state: &Self,
                    _doc: &#krate::Doc,
                    item: ::core::option::Option<&str>,
                ) -> ::core::result::Result<#krate::__private::serde_json::Value, ::std::string::String> {
                    let item = item.map(#krate::Id::from_raw);
                    #path(state, item.as_ref())
                }
            }
        },
    );
    let views = options.views.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                fn views(state: &Self) -> ::std::vec::Vec<#krate::__private::ResourceViewSpec> {
                    #path(state)
                }
            }
        },
    );

    let serde_path = format!("{CRATE}::__private::serde");
    let enum_doc =
        format!("The replayable actions of [`{type_name}`], one per `#[action]` method.");
    let module_doc = format!("The parameters of each action of [`{type_name}`].");
    let verb_doc = format!("Each action of [`{type_name}`], for its availability.");
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
                #[allow(unreachable_patterns, reason = "edits and the seed need writing")]
                match &action.operation {
                    #(#permissions)*
                    _ => #krate::__private::PermissionRequirement::Write,
                }
            }

            #least_fn
        }
    };
    let (record_binding, cx_binding) = if arms.is_empty() && options.seed.is_none() {
        (quote!(_record), quote!(_cx))
    } else {
        (quote!(record), quote!(cx))
    };

    Ok(quote! {
        #item

        #[doc = #module_doc]
        #[allow(unused_imports, reason = "the parameters name the types the methods do")]
        pub mod #params_mod {
            use super::*;

            #(#structs)*

            #[doc = #verb_doc]
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            pub enum Verb {
                #(#verb_variants,)*
            }
        }

        #(#item_checks)*

        #[doc = #enum_doc]
        #[derive(Clone, Debug, #krate::__private::serde::Serialize, #krate::__private::serde::Deserialize)]
        #[serde(crate = #serde_path, tag = "action", content = "args")]
        pub enum #action_enum {
            #(#variants,)*
            #seed_variant
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
            fn kind(&self) -> #krate::ActionKind {
                #[allow(unreachable_patterns, reason = "edits and the seed change only the document")]
                match self {
                    #(#kinds)*
                    _ => #krate::ActionKind::Undoable,
                }
            }
            fn perform(
                &self,
                #record_binding: &mut #self_ty,
                #cx_binding: &mut #krate::ActionCx,
            ) -> ::core::result::Result<(), #krate::Refusal> {
                match self {
                    #(#arms)*
                    #seed_arm
                    Self::FieldEdit(_) => ::core::result::Result::Err(#krate::Refusal::new(
                        "a field edit applies at its path",
                    )),
                }
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
                #krate::evolution::check_version::<Self>(doc)?;
                <Self as #krate::Fields>::read_fields(doc, &#krate::__private::ROOT)
            }

            fn refresh(
                state: &mut Self,
                doc: &#krate::Doc,
                delta: &#krate::Delta,
            ) -> ::core::result::Result<(), #krate::ModelError> {
                #krate::evolution::check_version::<Self>(doc)?;
                <Self as #krate::Fields>::refresh_fields(state, doc, &#krate::__private::ROOT, delta)
            }

            fn initialize(doc: &mut #krate::Doc) -> ::core::result::Result<(), #krate::ModelError> {
                let initial: Self = #initial;
                <Self as #krate::Fields>::write_fields(&initial, doc, &#krate::__private::ROOT)?;
                #krate::evolution::write_version(doc, <Self as #krate::Resource>::VERSION)
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

            #seed_fn
            #version
            #availability
            #read_content
            #views
            #permission_fn

            fn children(state: &Self) -> ::std::vec::Vec<#krate::__private::Resource> {
                let _ = state;
                #children
            }

            fn child(
                state: &Self,
                id: &str,
            ) -> ::core::option::Option<#krate::__private::Resource> {
                let _ = (state, id);
                #derived_child
                #item_child
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
