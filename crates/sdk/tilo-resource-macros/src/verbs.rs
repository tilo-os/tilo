//! `#[resource]`: a record's verbs, their parameter structs and the
//! resource implementation.

use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, FnArg, ImplItem, ItemImpl, Lit, Meta, Pat, Token, Type};

use crate::{CRATE, doc_lines, humanize, is_action_cx, krate, last_ident, owned_type, snake_case};

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
    document_child: Option<syn::Path>,
    /// A read that looks at the document too: `fn(&Self, &Doc, Option<&Id>)`.
    read_content_in_doc: Option<syn::Path>,
    /// The record's facts: `fn(&Self) -> Result<Value, String>`.
    facts: Option<syn::Path>,
    views: Option<syn::Path>,
    version: Option<syn::LitInt>,
    migrate: Option<syn::Path>,
    load: Option<syn::Path>,
    locate: Option<syn::Path>,
    lands: Option<syn::Path>,
    /// What every change of the record queues for the world outside.
    queue: Option<syn::Path>,
    effects: Option<syn::Path>,
    answer: Option<syn::Path>,
    /// The views each item of a collection offers.
    item_views: Option<syn::Path>,
}

const RESOURCE_KEYS: &str = "expected `kind = \"...\"`, `label = \"...\"`, `params = name`, `search = path`, \
     `scopes = path`, `children = path`, `initialize = path`, `seed = path`, `available = path`, \
     `read_content = path`, `read_content_in_doc = path`, `document_child = path`, `facts = path`, `views = path`, `version = N`, `migrate = path`, `load = path`, \
     `locate = path`, `lands = path`, `queue = path`, `effects = Type`, `answer = path` or \
     `item_views = path`";

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
        document_child: None,
        read_content_in_doc: None,
        facts: None,
        views: None,
        version: None,
        migrate: None,
        load: None,
        locate: None,
        lands: None,
        queue: None,
        effects: None,
        answer: None,
        item_views: None,
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
            ("document_child", Expr::Path(path)) => {
                options.document_child = Some(path.path.clone());
            }
            ("read_content_in_doc", Expr::Path(path)) => {
                options.read_content_in_doc = Some(path.path.clone());
            }
            ("facts", Expr::Path(path)) => options.facts = Some(path.path.clone()),
            ("views", Expr::Path(path)) => options.views = Some(path.path.clone()),
            (
                "version",
                Expr::Lit(ExprLit {
                    lit: Lit::Int(version),
                    ..
                }),
            ) => options.version = Some(version.clone()),
            ("migrate", Expr::Path(path)) => options.migrate = Some(path.path.clone()),
            ("load", Expr::Path(path)) => options.load = Some(path.path.clone()),
            ("locate", Expr::Path(path)) => options.locate = Some(path.path.clone()),
            ("lands", Expr::Path(path)) => options.lands = Some(path.path.clone()),
            ("queue", Expr::Path(path)) => options.queue = Some(path.path.clone()),
            ("effects", Expr::Path(path)) => options.effects = Some(path.path.clone()),
            ("answer", Expr::Path(path)) => options.answer = Some(path.path.clone()),
            ("item_views", Expr::Path(path)) => options.item_views = Some(path.path.clone()),
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
    field: Option<String>,
    destructive: bool,
    irreversible: bool,
    housekeeping: bool,
    external: bool,
    /// Queues nothing though its record queues: an undoable change of the
    /// record alone (mail a room holds, which no server keeps).
    local: bool,
    /// `local = path`: `fn(&params) -> bool`, whether one invocation stays
    /// in the record (an event on the local calendar) or queues (one on a
    /// remote calendar), decided from its arguments.
    local_if: Option<syn::Path>,
    incoming_message: bool,
    answers: bool,
    item: Option<syn::Ident>,
    /// The collections the item's collection sits in (`item = slides.elements`).
    within: Vec<syn::Ident>,
    /// The function that names the verb compensating this one's outside
    /// consequence on undo.
    undo: Option<syn::ExprPath>,
    writes: Vec<syn::Ident>,
    /// Resolves arguments from outside once, where the verb is invoked.
    prepare: Option<syn::Path>,
    /// `audience = "agents"` or `"people"`: who listings offer it to.
    audience: Option<syn::Ident>,
}

fn action_options(attr: &Attribute) -> syn::Result<ActionOptions> {
    let mut options = ActionOptions {
        id: None,
        label: None,
        field: None,
        destructive: false,
        irreversible: false,
        housekeeping: false,
        external: false,
        local: false,
        local_if: None,
        incoming_message: false,
        answers: false,
        item: None,
        within: Vec::new(),
        undo: None,
        writes: Vec::new(),
        prepare: None,
        audience: None,
    };
    if matches!(attr.meta, Meta::Path(_)) {
        return Ok(options);
    }
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("id") {
            options.id = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("label") {
            options.label = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("field") {
            options.field = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("moderation") {
            // Acting on other people's content: placed under the `moderation`
            // field node (`tilo_action_policy::roles::MODERATION_FIELD`), which
            // role presets allow or deny with one rule.
            if options.field.is_some() {
                return Err(meta.error(
                    "a `moderation` action sits under the `moderation` field; name no other `field`",
                ));
            }
            options.field = Some("moderation".to_owned());
        } else if meta.path.is_ident("destructive") {
            options.destructive = true;
        } else if meta.path.is_ident("irreversible") {
            options.irreversible = true;
        } else if meta.path.is_ident("housekeeping") {
            options.housekeeping = true;
        } else if meta.path.is_ident("incoming_message") {
            options.incoming_message = true;
        } else if meta.path.is_ident("external") {
            options.external = true;
        } else if meta.path.is_ident("local") {
            if meta.input.peek(Token![=]) {
                options.local_if = Some(meta.value()?.parse::<syn::Path>()?);
            } else {
                options.local = true;
            }
        } else if meta.path.is_ident("item") {
            let value = meta.value()?;
            let mut names = vec![value.parse::<syn::Ident>()?];
            while value.peek(Token![.]) {
                value.parse::<Token![.]>()?;
                names.push(value.parse::<syn::Ident>()?);
            }
            options.item = names.pop();
            options.within = names;
        } else if meta.path.is_ident("undo") {
            options.undo = Some(meta.value()?.parse::<syn::ExprPath>()?);
        } else if meta.path.is_ident("answers") {
            options.answers = true;
        } else if meta.path.is_ident("prepare") {
            options.prepare = Some(meta.value()?.parse::<syn::Path>()?);
        } else if meta.path.is_ident("writes") {
            options.writes.push(meta.value()?.parse::<syn::Ident>()?);
        } else if meta.path.is_ident("audience") {
            options.audience = Some(crate::audience_option(&meta)?);
        } else {
            return Err(meta.error(
                "expected `id = \"...\"`, `label`, `field`, `moderation`, `destructive`, `irreversible`, `housekeeping`, `external`, `local`, `incoming_message`, `answers`, `writes = field`, `prepare = path`, `item = field`, `audience = \"agents\"` or `undo = path`",
            ));
        }
        if options.housekeeping && options.external {
            return Err(meta.error("an action is `housekeeping` or `external`, not both"));
        }
        Ok(())
    })?;
    Ok(options)
}

#[cfg(test)]
mod tests;

/// `#[param("description", name = "public", example = expr)]`, each part
/// optional.
#[derive(Default)]
struct ParamOptions {
    description: Option<String>,
    name: Option<String>,
    example: Option<Expr>,
    secret: bool,
    /// The parameter's role, with content taking precedence over flatten and place.
    role: Role,
    /// The collection whose items the place names (`place = "field"`).
    place_in: Option<String>,
}

impl ParamOptions {
    /// Keep the established precedence regardless of attribute order.
    fn add_role(&mut self, role: Role) {
        self.role = match (self.role, role) {
            (Role::Content, _) | (_, Role::Content) => Role::Content,
            (Role::Flatten, _) | (_, Role::Flatten) => Role::Flatten,
            (Role::Place, _) | (_, Role::Place) => Role::Place,
            (Role::Plain, Role::Plain) => Role::Plain,
        };
    }
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
            if input.peek(syn::LitStr) {
                let text = input.parse::<syn::LitStr>()?;
                if options.description.replace(text.value()).is_some() {
                    return Err(syn::Error::new(text.span(), "one parameter description"));
                }
                if input.is_empty() {
                    break;
                }
                input.parse::<Token![,]>()?;
                continue;
            }
            let key: syn::Ident = input.parse()?;
            // `place` alone is a flag; `place = "field"` names its collection.
            let flag = key == "content" || key == "flatten" || key == "place" || key == "secret";
            if flag && !input.peek(Token![=]) {
                if key == "secret" {
                    options.secret = true;
                } else if key == "content" {
                    options.add_role(Role::Content);
                } else if key == "place" {
                    options.add_role(Role::Place);
                } else {
                    options.add_role(Role::Flatten);
                }
                if input.is_empty() {
                    break;
                }
                input.parse::<Token![,]>()?;
                continue;
            }
            input.parse::<Token![=]>()?;
            if key == "place" {
                options.add_role(Role::Place);
                options.place_in = Some(input.parse::<syn::LitStr>()?.value());
            } else if key == "name" {
                options.name = Some(input.parse::<syn::LitStr>()?.value());
            } else if key == "example" {
                options.example = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `\"description\"`, `name = \"...\"`, `example = ...`, `place = \"field\"`, `content`, `place`, `flatten` or `secret`",
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
    /// The host must retain the secret argument variant and redact diagnostics.
    secret: bool,
    /// What part the parameter plays beyond carrying a value.
    role: Role,
    /// The collection a place parameter's items come from, when named.
    place_in: Option<String>,
}

/// What part a verb's parameter plays.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum Role {
    /// A value.
    #[default]
    Plain,
    /// Content another action produced, carried by its callers as a hash.
    Content,
    /// A struct whose fields are the action's parameters
    /// (`#[param(flatten)]`, the only parameter).
    Flatten,
    /// A place inside the record the verb acts at.
    Place,
}

enum Input {
    Cx,
    /// An outbox of the record, by its field.
    Outbox(syn::Ident),
    Param(usize),
    /// The item an item method acts on (`fn star(contact: &mut Contact)`),
    /// named by its id parameter.
    Item,
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
    /// An item method's item: its id parameter and the item's type.
    item: Option<(syn::Ident, Type)>,
}

/// The rules a verb's parameters keep: at most one content parameter, a
/// `String` holding the content's hash, beside any others; a flattened
/// parameter is the only one.
fn check_params(params: &[Param], span: Span) -> syn::Result<()> {
    for param in params.iter().filter(|param| param.secret) {
        if param.role != Role::Plain
            || last_ident(crate::schema_param_type(&param.ty))
                .is_none_or(|(name, _)| name != "String")
            || param.example.is_some()
        {
            return Err(syn::Error::new(
                span,
                "a secret parameter is String or Option<String>, without a public example, content, place or flatten",
            ));
        }
    }
    let content: Vec<&Param> = params
        .iter()
        .filter(|param| param.role == Role::Content)
        .collect();
    if content.len() > 1
        || content
            .iter()
            .any(|param| last_ident(&param.ty).is_none_or(|(name, _)| name != "String"))
    {
        return Err(syn::Error::new(
            span,
            "a verb takes at most one `#[param(content)]`, a `String` holding the content's hash",
        ));
    }
    if params.iter().any(|param| param.role == Role::Flatten) && params.len() != 1 {
        return Err(syn::Error::new(
            span,
            "a `#[param(flatten)]` is the action's only parameter: its fields are the parameters",
        ));
    }
    Ok(())
}

/// An action takes `&mut self`, or as an item method its item and the
/// collection it is in.
fn check_receiver(
    takes_self: bool,
    item_method: bool,
    names_collection: bool,
    span: Span,
) -> syn::Result<()> {
    if !takes_self && !item_method {
        return Err(syn::Error::new(
            span,
            "an action takes `&mut self`, or as an item method the item (`item: &mut Item`) with `#[action(item = field)]`",
        ));
    }
    if item_method && !names_collection {
        return Err(syn::Error::new(
            span,
            "an item method names its collection: `#[action(item = field)]`",
        ));
    }
    Ok(())
}

/// The id parameter an item method's item is named by.
fn item_param(ident: &syn::Ident) -> Param {
    let krate = krate();
    let name = ident.to_string();
    Param {
        ident: ident.clone(),
        public: name.clone(),
        ty: syn::parse_quote!(#krate::Id),
        by_ref: true,
        description: format!("The {} it acts on", humanize(&name)),
        example: None,
        secret: false,
        role: Role::Plain,
        place_in: None,
    }
}

fn take_param_options(attrs: &mut Vec<Attribute>) -> syn::Result<ParamOptions> {
    if let Some(index) = attrs.iter().position(|attr| attr.path().is_ident("param")) {
        attrs.remove(index).parse_args()
    } else {
        Ok(ParamOptions::default())
    }
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
    let mut item = None;
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
                // An item method: its first parameter is the item itself.
                if !takes_self
                    && inputs.is_empty()
                    && let Type::Reference(reference) = &*typed.ty
                    && reference.mutability.is_some()
                {
                    item = Some((pat.ident.clone(), (*reference.elem).clone()));
                    inputs.push(Input::Item);
                    params.push(item_param(&pat.ident));
                    continue;
                }
                let param_options = take_param_options(&mut typed.attrs)?;
                let (owned, by_ref) = owned_type(&typed.ty);
                let name = pat.ident.to_string();
                let role = param_options.role;
                inputs.push(Input::Param(params.len()));
                params.push(Param {
                    ident: pat.ident.clone(),
                    public: param_options.name.unwrap_or_else(|| name.clone()),
                    ty: owned,
                    by_ref,
                    description: param_options.description.unwrap_or_else(|| humanize(&name)),
                    example: param_options.example,
                    secret: param_options.secret,
                    role,
                    place_in: param_options.place_in,
                });
            }
        }
    }
    check_receiver(
        takes_self,
        item.is_some(),
        options.item.is_some(),
        method.sig.span(),
    )?;
    check_params(&params, method.sig.span())?;
    Ok(Verb {
        method: method_ident,
        id,
        variant,
        label,
        description,
        options,
        inputs,
        params,
        item,
    })
}

/// What a verb declares of `param`, as a constant [`ParamDecl`].
fn param_decl(param: &Param, krate: &Tokens) -> Tokens {
    let name = &param.public;
    let ty = &param.ty;
    let description = &param.description;
    let flat = param.role == Role::Flatten;
    let (known, optional) = crate::known_param_type(ty);
    let param_ty = if param.secret {
        quote!(#krate::__private::ParamTypeOf::Known(#krate::__private::ParamType::Secret))
    } else {
        known.map_or_else(
            || {
                let schema_ty = crate::schema_param_type(ty);
                quote!(#krate::__private::ParamTypeOf::Schema(
                    <#schema_ty as #krate::JsonSchema>::json_schema
                ))
            },
            |known| quote!(#krate::__private::ParamTypeOf::Known(#known)),
        )
    };
    let example = param.example.as_ref().map_or_else(
        || quote!(::core::option::Option::None),
        |example| {
            quote!(::core::option::Option::Some(
                (|| #krate::__private::serde_json::json!(#example))
                    as fn() -> #krate::__private::serde_json::Value
            ))
        },
    );
    let place = match (&param.place_in, param.role == Role::Place) {
        (Some(field), _) => quote!(#krate::__private::PlaceIn::Field(#field)),
        (None, true) => quote!(#krate::__private::PlaceIn::Record),
        (None, false) => quote!(#krate::__private::PlaceIn::Nowhere),
    };
    let content = param.role == Role::Content;
    let schema = crate::byte_param(ty).map_or_else(
        || quote!(<#ty as #krate::JsonSchema>::json_schema),
        |(_, schema)| schema,
    );
    quote! {
        #krate::__private::ParamDecl {
            name: #name,
            ty: #param_ty,
            description: #description,
            schema: #schema,
            optional: #optional,
            place: #place,
            example: #example,
            flat: #flat,
            content: #content,
        }
    }
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
        if param.role == Role::Flatten {
            return quote! {
                #[doc = #description]
                #[serde(flatten)]
                pub #ident: #ty
            };
        }
        quote! {
            #[doc = #description]
            #rename
            #default
            pub #ident: #ty
        }
    });
    let param_decls = verb.params.iter().map(|param| param_decl(param, &krate));
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
        let schema = crate::byte_param(ty).map_or_else(
            || quote!(<#ty as #krate::JsonSchema>::json_schema()),
            |(_, schema)| quote!(#schema()),
        );
        quote! {(
            #public,
            #krate::__private::param_schema(
                #schema,
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
        if param.role == Role::Flatten {
            return quote!(#ident: #krate::Path::<#name, #name>::root().cast());
        }
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
    let reads = verb.params.iter().map(|param| {
        let ident = &param.ident;
        let public = &param.public;
        if param.role == Role::Flatten {
            return quote!(#ident: #krate::__private::serde::Deserialize::deserialize(
                #krate::__private::serde_json::Value::Object(args),
            )?);
        }
        if let Some((decode, _)) = crate::byte_param(&param.ty) {
            return quote!(#ident: #krate::__private::bytes_arg(&mut args, #public, #decode)?);
        }
        let optional = last_ident(&param.ty).is_some_and(|(name, _)| name == "Option");
        quote!(#ident: #krate::__private::arg(&mut args, #public, #optional)?)
    });
    let label = &verb.label;
    let schema_body = verb
        .params
        .first()
        .filter(|param| param.role == Role::Flatten)
        .map_or_else(
            || {
                quote! {
                    #krate::schema::describe(
                        #krate::schema::object(::std::vec![#(#schemas),*]),
                        #label,
                    )
                }
            },
            |param| {
                let ty = &param.ty;
                quote!(#krate::schema::describe(<#ty as #krate::JsonSchema>::json_schema(), #label))
            },
        );
    quote! {
        #[doc = #doc]
        #[derive(Clone, Debug, #derive_default #krate::__private::serde::Serialize)]
        #[serde(crate = #serde_path)]
        pub struct #name {
            #(#fields,)*
        }

        // Read through a JSON value, each argument by its own type's decoder:
        // a verb's parameters add no decoder of their own.
        impl<'de> #krate::__private::serde::Deserialize<'de> for #name {
            fn deserialize<D: #krate::__private::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                let value = <#krate::__private::serde_json::Value as #krate::__private::serde::Deserialize>::deserialize(deserializer)?;
                match Self::from_args(value) {
                    ::core::result::Result::Ok(params) => ::core::result::Result::Ok(params),
                    ::core::result::Result::Err(error) => ::core::result::Result::Err(
                        <D::Error as #krate::__private::serde::de::Error>::custom(error),
                    ),
                }
            }
        }

        impl #name {
            /// What the verb declares of its parameters.
            #[doc(hidden)]
            pub const __PARAMS: &'static [#krate::__private::ParamDecl] = &[#(#param_decls),*];

            /// The parameters in their JSON form, an object of arguments.
            #[inline(never)]
            fn from_args(
                value: #krate::__private::serde_json::Value,
            ) -> ::core::result::Result<Self, #krate::__private::serde_json::Error> {
                #[allow(unused_mut, unused_variables, reason = "a verb may take no arguments")]
                let mut args = #krate::__private::args_object(value)?;
                ::core::result::Result::Ok(Self { #(#reads,)* })
            }
        }

        impl #krate::JsonSchema for #name {
            fn json_schema() -> #krate::__private::serde_json::Value {
                #schema_body
            }
        }

        #conversions

        impl #krate::Params for #name {
            type Resource = #self_ty;
            type Paths = #paths_name;
            const ACTION: #krate::ActionRef<#self_ty, Self> = <#self_ty>::#const_ident;

            fn paths() -> #paths_name {
                #paths_name { #(#path_inits,)* }
            }
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
                <Self as #krate::Params>::paths()
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
    let mut decode_arms = Vec::new();
    let mut arms = Vec::new();
    let mut risks = Vec::new();
    let mut kinds = Vec::new();
    let mut fields_of = Vec::new();
    let mut change_kinds = Vec::new();
    let mut incoming = Vec::new();
    let mut irreversibles = Vec::new();
    let mut compensations = Vec::new();
    let mut item_arms = Vec::new();
    let mut compensating = Vec::new();
    let mut consts = Vec::new();
    let mut decls = Vec::new();
    let mut item_actions = Vec::new();
    let mut writes_arms = Vec::new();
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
        decode_arms.push(quote! {
            #id => #krate::__private::serde::Deserialize::deserialize(args).map(Self::#variant),
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
        let call_args: Vec<Tokens> = verb
            .inputs
            .iter()
            .map(|input| match input {
                Input::Item => quote!(__item),
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
            })
            .collect();
        if let Some((ident, item_ty)) = &verb.item {
            if !outboxes.is_empty() {
                return Err(syn::Error::new(
                    verb.method.span(),
                    "an item method changes its item only; a verb that queues takes `&mut self`",
                ));
            }
            let field = verb
                .options
                .item
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            let field_ident = format_ident!("{}", field);
            arms.push(quote! {
                Self::#variant(params) => {
                    let _ = &params;
                    let __item = record
                        .#field_ident
                        .get_mut(&params.#ident)
                        .ok_or_else(|| #krate::Refusal::new("no such item"))?;
                    #krate::Outcome::into_result(<#self_ty>::#method(#(#call_args),*))
                }
            });
            item_arms.push(quote! {
                Self::#variant(params) => ::core::option::Option::Some(
                    #krate::__private::apply_item::<#self_ty, #item_ty>(
                        doc,
                        cx,
                        &[
                            #krate::Segment::Field(::std::string::String::from(#field)),
                            #krate::Segment::Item(::core::clone::Clone::clone(&params.#ident)),
                        ],
                        |__item, cx| {
                            let _ = &cx;
                            #krate::Outcome::into_result(<#self_ty>::#method(#(#call_args),*))
                        },
                    ),
                ),
            });
        } else {
            arms.push(quote! {
                Self::#variant(params) => {
                    let _ = &params;
                    #(let mut #taken = ::core::mem::take(&mut record.#outboxes);)*
                    let outcome = #krate::Outcome::into_result(record.#method(#(#call_args),*));
                    #(record.#outboxes = #taken;)*
                    outcome
                }
            });
        }
        let queues = !outboxes.is_empty() || options.queue.is_some();
        let local = verb.options.local || verb.options.local_if.is_some();
        if local && !outboxes.is_empty() {
            return Err(syn::Error::new(
                method.span(),
                "a `local` action queues nothing itself: it takes no `&mut Outbox`",
            ));
        }
        // Decided by its arguments: declared as the least it is.
        let by_arguments = queues && verb.options.local_if.is_some();
        let action_kind = if queues && !local {
            quote!(Outside)
        } else {
            quote!(Undoable)
        };
        if let Some(undo) = &verb.options.undo {
            if !queues {
                return Err(syn::Error::new(
                    undo.span(),
                    "`undo` names the verb that compensates an outside consequence; this action queues none (it takes no `&mut Outbox` and the record has no `queue`) and undoes by itself",
                ));
            }
            compensating.push(variant.clone());
            compensations.push(quote! {
                #action_enum::#variant(params) => (#undo)(#krate::Undoing {
                    before,
                    after,
                    minted,
                    params,
                })
                .map(|invoke| invoke.build(0).map(|action| action.operation)),
            });
        }
        match (&verb.options.local_if, by_arguments) {
            (Some(stays), true) => {
                kinds.push(quote!(Self::#variant(params) => if (#stays)(params) {
                #krate::ActionKind::Undoable
            } else {
                #krate::ActionKind::Outside
            },));
            }
            _ => kinds.push(quote!(Self::#variant(..) => #krate::ActionKind::#action_kind,)),
        }
        if verb.options.destructive {
            risks.push(quote!(Self::#variant(..) => #krate::__private::RiskLevel::Destructive,));
        } else if verb.options.irreversible {
            // A declaration that says only that Undo cannot take it back
            // cannot say the invocation is harmless, so it reaches a person.
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
        if verb.options.incoming_message {
            incoming.push(quote!(Self::#variant(..) => true,));
        }
        if verb.options.irreversible {
            irreversibles.push(quote!(Self::#variant(..) => true,));
        }

        let const_ident = format_ident!("{}", method.to_string().to_uppercase());
        let const_doc = format!("Bind or apply the `{id}` action: {label_text}.");
        consts.push(quote! {
            #[doc = #const_doc]
            pub const #const_ident: #krate::ActionRef<#self_ty, #params_mod::#variant> =
                #krate::ActionRef::new(#id, &[]);
        });

        // What the verb may write: its item's unit and the fields it
        // declares, beside the record unit.
        let declared_writes: Vec<String> = verb
            .options
            .writes
            .iter()
            .map(ToString::to_string)
            .collect();
        let item_param = verb
            .params
            .first()
            .filter(|first| last_ident(&first.ty).is_some_and(|(name, _)| name == "Id"));
        let writes_item = item_param.map_or_else(
            || quote!(::core::option::Option::None),
            |first| {
                let ident = &first.ident;
                quote!(::core::option::Option::Some(::core::clone::Clone::clone(&params.#ident)))
            },
        );
        let item_field = item_param.map_or_else(
            || quote!(::core::option::Option::None),
            |first| {
                let param = &first.public;
                let named = verb
                    .options
                    .item
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                quote!(#krate::items::item_field(
                    <#self_ty as #krate::Fields>::ITEM_FIELDS,
                    #param,
                    #named,
                ))
            },
        );
        writes_arms.push(quote! {
            Self::#variant(params) => {
                let _ = &params;
                #krate::footprint::Writes {
                    item: #writes_item,
                    fields: &[#(#declared_writes),*],
                }
            }
        });

        // An action whose first parameter is an item id acts on an element
        // of a collection: it is also offered on each item, id bound. One
        // naming an outbox (`item = ops`) acts on an entry of it, which is
        // no item resource.
        if !verb.options.within.is_empty() {
            // An item of a collection inside items: the first parameters
            // name the enclosing items, outermost first, then the item.
            let depth = verb.options.within.len();
            let ids = verb
                .params
                .iter()
                .take(depth.saturating_add(1))
                .filter(|param| last_ident(&param.ty).is_some_and(|(name, _)| name == "Id"))
                .count();
            if ids != depth.saturating_add(1) {
                return Err(syn::Error::new(
                    verb.method.span(),
                    "an action on an item of a nested collection takes the enclosing items' ids, then the item's, first",
                ));
            }
            let outer: Vec<String> = verb
                .options
                .within
                .iter()
                .map(ToString::to_string)
                .collect();
            let outer_params: Vec<&String> = verb
                .params
                .iter()
                .take(depth)
                .map(|param| &param.public)
                .collect();
            let item_param = verb.params.get(depth).map(|param| &param.public);
            let named = verb
                .options
                .item
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            item_actions.push(quote!(#krate::items::ItemAction {
                action: #id,
                param: #item_param,
                collection: ::core::option::Option::Some(#named),
                within: &[#(#outer),*],
                within_params: &[#(#outer_params),*],
            }));
        } else if let Some(first) = verb.params.first()
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
                within: &[],
                within_params: &[],
            }));
            let message = if named.is_empty() {
                format!(
                    "`{}` acts on an item, but no collection of `{type_name}` is named `{}` and it has more than one: name it with `#[action(item = field)]`",
                    verb.method, first.public
                )
            } else {
                format!("`{type_name}` has no collection or outbox named `{named}`")
            };
            item_checks.push(quote! {
                const _: () = ::core::assert!(
                    #krate::items::item_target(<#self_ty as #krate::Fields>::ITEM_FIELDS, #param, #named)
                        || #krate::items::entry_target(<#self_ty>::__OUTBOXES, #named),
                    #message,
                );
            });
        }

        let destructive = verb.options.destructive;
        let irreversible = verb.options.irreversible;
        let housekeeping = verb.options.housekeeping;
        let answer = if verb.options.answers {
            quote!(Made)
        } else if outboxes.is_empty() {
            quote!(Created)
        } else {
            quote!(Queued)
        };
        let incoming_message = verb.options.incoming_message;
        let audience = verb
            .options
            .audience
            .clone()
            .unwrap_or_else(|| syn::Ident::new("Everyone", proc_macro2::Span::call_site()));
        let undo = if verb.options.undo.is_some() {
            quote!(Compensation)
        } else {
            quote!(Revert)
        };
        // Run where the verb is built from its arguments (the origin), never
        // when its recorded action replays: the action carries the result.
        let prepare = verb
            .options
            .prepare
            .as_ref()
            .map(|path| quote!(#path(&mut params)?;));
        // Without a `prepare` hook, the arguments decode as a recorded
        // action does: one shared decoder per resource, not one per verb.
        let build_body = match &prepare {
            None => {
                quote!(#krate::__private::build_stamped::<#action_enum>(#id, &arguments, now_ms))
            }
            Some(prepare) => quote! {
                let mut params: #params_mod::#variant = #krate::__private::serde_json::from_value(
                    #krate::__private::serde_json::Value::Object(arguments),
                )
                .map_err(|error| ::std::string::ToString::to_string(&error))?;
                #prepare
                ::core::result::Result::Ok(#krate::Stamped {
                    operation: #action_enum::#variant(params),
                    now_ms,
                })
            },
        };
        // A verb whose one input is content is published as collecting it
        // (its callers carry the hash in); one that takes content beside
        // other parameters publishes them all, the content one marked.
        let only_content = verb.params.len() == 1;
        let (collects, prompted, content) = match verb
            .params
            .iter()
            .find(|param| param.role == Role::Content && only_content)
        {
            Some(param) => {
                let description = &param.description;
                let public = &param.public;
                (
                    quote!(::core::option::Option::Some(#description)),
                    quote! {
                        if let ::core::option::Option::Some(hash) =
                            arguments.remove(#krate::__private::PROMPTED_PARAM)
                        {
                            arguments.insert(::std::string::String::from(#public), hash);
                        }
                    },
                    quote!(::core::option::Option::Some(#public)),
                )
            }
            None => (
                quote!(::core::option::Option::None),
                quote!(),
                quote!(::core::option::Option::None),
            ),
        };
        let description = &verb.description;
        let field = verb.options.field.as_deref().map_or_else(
            || quote!(::core::option::Option::None),
            |field| {
                fields_of.push(quote! {
                    #action_enum::#variant(..) => ::core::option::Option::Some(#field),
                });
                quote!(::core::option::Option::Some(#field))
            },
        );
        let verb_decl = quote! {
            #krate::__private::VerbDecl {
                id: #id,
                label: #label_text,
                description: #description,
                kind: #krate::ActionKind::#action_kind,
                by_arguments: #by_arguments,
                field: #field,
                destructive: #destructive,
                irreversible: #irreversible,
                housekeeping: #housekeeping,
                answer: #krate::__private::VerbAnswer::#answer,
                incoming_message: #incoming_message,
                undo: #krate::__private::VerbUndo::#undo,
                audience: #krate::__private::Audience::#audience,
                collects: #collects,
                content: #content,
                item_field: #item_field,
                writes: &[#(#declared_writes),*],
                shape: <#self_ty as #krate::Model>::SHAPE,
                params: #params_mod::#variant::__PARAMS,
            }
        };
        // Without a `prepare` hook, every verb of the record is built by one
        // function: its arguments decoded as a recorded action is.
        decls.push(if prepare.is_some() {
            quote! {
                #krate::__private::declare::<#self_ty>(#verb_decl, |args, now_ms| {
                    let mut arguments = args.to_json();
                    let _ = &mut arguments;
                    #prompted
                    #build_body
                })
            }
        } else {
            quote! {
                #krate::__private::declare_verb::<#self_ty, #action_enum>(#verb_decl)
            }
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
    // The record's own fields that hold resources (`#[model(resource)]`),
    // then its derived places, published again only when they changed.
    let published = options.children.as_ref().map_or_else(
        || quote!(#krate::derived::published_nested::<Self>()),
        |path| quote!(#krate::derived::published::<Self>(#path(state))),
    );
    let derived_child = quote! {
        if let ::core::option::Option::Some(found) = (#published).find(id) {
            return ::core::option::Option::Some(found);
        }
    };
    // Items are published when an item verb or effect is offered on them
    // or they carry a label; addressed one by one beyond the page.
    let item_child = quote! {
        #krate::items::item_child::<Self>(
            id,
            <Self as #krate::Fields>::ITEM_FIELDS,
            &[#(#item_actions),*],
            |field, item| <Self as #krate::Fields>::item_label(state, field, item),
        )
    };
    // The declarations offered on items are known from which collections
    // hold items, so the record's own actions publish without a child per
    // item ([`Resource::children_offer_bound`]).
    let offer_bound = quote! {
        fn children_offer_bound(state: &Self) -> ::std::vec::Vec<&'static str> {
            #krate::items::offered_on_items::<Self>(
                &[],
                &#published,
                &<Self as #krate::Fields>::item_collection_sizes(state),
                &[#(#item_actions),*],
                &|action| #krate::__private::holds_nested_items(&Self::__RECORD, state, action),
            )
        }
    };
    // The same parts as `children`, apart, so the framework can keep the
    // derived and item children it built for an earlier version.
    let children_parts = quote! {
        fn children_parts(state: &Self) -> #krate::items::ChildrenParts {
            #krate::items::ChildrenParts {
                others: ::std::vec::Vec::new(),
                kept: ::std::rc::Rc::clone((#published).resources()),
                items: <Self as #krate::Fields>::item_collections(state),
                item_actions: ::std::vec![#(#item_actions),*],
            }
        }
    };
    let children = quote! {{
        let mut children: ::std::vec::Vec<#krate::__private::Resource> =
            (#published).to_resources();
        children.extend(#krate::items::item_children::<Self>(
            &<Self as #krate::Fields>::item_collections(state),
            &[#(#item_actions),*],
        ));
        children
    }};
    let item_effects = options.effects.as_ref().map_or_else(
        || quote!(),
        |effects| {
            quote! {
                fn item_effects(
                    field: &str,
                    item: &#krate::Id,
                ) -> ::std::vec::Vec<#krate::__private::ActionSpec> {
                    <#effects as #krate::Effect>::item_specs(field, item)
                }
            }
        },
    );
    let item_views = options.item_views.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                fn item_views(
                    field: &str,
                    item: &#krate::Id,
                ) -> ::std::vec::Vec<#krate::__private::ResourceViewSpec> {
                    #path(field, item)
                }
            }
        },
    );
    let answer = options.answer.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                fn answer(
                    doc: &#krate::Doc,
                    action: &str,
                    changes: &[#krate::__private::Patch],
                ) -> ::core::option::Option<#krate::__private::serde_json::Value> {
                    #path(doc, action, changes)
                }
            }
        },
    );
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
    let seed_decode = options
        .seed
        .as_ref()
        .map(|_| quote!("seed" => ::core::result::Result::Ok(Self::Seed),));
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
    let load = options.load.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                fn load(
                    state: &mut Self,
                    doc: &#krate::Doc,
                    demand: &#krate::Demand,
                ) -> ::core::result::Result<(), #krate::ModelError> {
                    #path(state, doc, demand)
                }
            }
        },
    );
    let locate = options.locate.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                fn locate(
                    state: &Self,
                    obj: &#krate::__private::ObjId,
                ) -> ::core::option::Option<#krate::ObjPath> {
                    #path(state, obj)
                }
            }
        },
    );
    let lands_first = options.lands.as_ref().map_or_else(
        || quote!(),
        |path| {
            quote! {
                if let ::core::option::Option::Some(place) = #path(state, action) {
                    return ::core::option::Option::Some(place);
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
    let read_content = options.read_content_in_doc.as_ref().map_or(read_content, |path| {
        quote! {
            const READABLE: bool = true;
            fn read_content(
                state: &Self,
                doc: &#krate::Doc,
                item: ::core::option::Option<&str>,
            ) -> ::core::result::Result<#krate::__private::serde_json::Value, ::std::string::String> {
                let item = item.map(#krate::Id::from_raw);
                #path(state, doc, item.as_ref())
            }
        }
    });
    let facts = options.facts.as_ref().map(|path| {
        quote! {
            const HAS_FACTS: bool = true;
            fn facts(
                state: &Self,
                _doc: &#krate::Doc,
            ) -> ::core::result::Result<#krate::__private::serde_json::Value, ::std::string::String> {
                #path(state)
            }
        }
    });
    let document_child = options.document_child.as_ref().map(|path| {
        quote! {
            fn document_child(state: &Self, id: &str) -> bool {
                #path(state, id)
            }
        }
    });
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
    let risk = quote! {
        fn risk(&self) -> #krate::__private::RiskLevel {
            #[allow(unreachable_patterns, reason = "every other operation is low risk")]
            match self {
                #(#risks)*
                Self::FieldEdit(edit) => edit.risk(),
                _ => #krate::__private::RiskLevel::Low,
            }
        }
    };
    // Item methods read and write their item alone, unless the record holds
    // an invariant of its own, which needs the whole record.
    let item_fn = if item_arms.is_empty() {
        quote!()
    } else {
        quote! {
            fn perform_item(
                &self,
                doc: &mut #krate::Doc,
                cx: &mut #krate::ActionCx,
            ) -> ::core::option::Option<::core::result::Result<(), #krate::Refusal>> {
                if <#self_ty>::__OWN_CHECK {
                    return ::core::option::Option::None;
                }
                #[allow(unreachable_patterns, reason = "only some verbs are item methods")]
                match self {
                    #(#item_arms)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    };
    // A record whose every change may queue an outside consequence: each of
    // its actions reaches outside.
    let (default_kind, queue_fns, queue_inherent) = match &options.queue {
        Some(path) => (
            quote!(Outside),
            quote! {
                const QUEUES: bool = true;
                fn queue(
                    record: &mut #self_ty,
                    before: &#self_ty,
                    cx: &mut #krate::ActionCx,
                ) {
                    <#self_ty>::__queue(record, before, cx);
                }
            },
            quote! {
                #[doc(hidden)]
                fn __queue(record: &mut Self, before: &Self, cx: &mut #krate::ActionCx) {
                    #path(record, before, cx);
                }
            },
        ),
        None => (quote!(Undoable), quote!(), quote!()),
    };
    let (compensation_fns, compensation_inherent) = if compensations.is_empty() {
        (quote!(), quote!())
    } else {
        (
            quote! {
                fn compensates(&self) -> bool {
                    ::core::matches!(self, #(Self::#compensating(..))|*)
                }
                fn compensation(
                    &self,
                    before: &#self_ty,
                    after: &#self_ty,
                    minted: &[#krate::Id],
                ) -> ::core::option::Option<::core::result::Result<Self, ::std::string::String>> {
                    <#self_ty>::__compensation(self, before, after, minted)
                }
            },
            quote! {
                #[doc(hidden)]
                fn __compensation(
                    operation: &#action_enum,
                    before: &Self,
                    after: &Self,
                    minted: &[#krate::Id],
                ) -> ::core::option::Option<::core::result::Result<#action_enum, ::std::string::String>> {
                    #[allow(unreachable_patterns, reason = "only some verbs declare an undo")]
                    match operation {
                        #(#compensations)*
                        _ => ::core::option::Option::None,
                    }
                }
            },
        )
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
    let irreversible_fn = if irreversibles.is_empty() {
        quote!()
    } else {
        quote! {
            fn irreversible(&self) -> bool {
                #[allow(unreachable_patterns, reason = "Undo takes every other operation back")]
                match self {
                    #(#irreversibles)*
                    _ => false,
                }
            }
        }
    };
    let incoming_fn = if incoming.is_empty() {
        quote!()
    } else {
        quote! {
            fn incoming_message(&self) -> bool {
                #[allow(unreachable_patterns, reason = "every other operation records no received message")]
                match self {
                    #(#incoming)*
                    _ => false,
                }
            }
        }
    };
    // An action declared below one field is placed there whichever door it
    // comes through, the generic `apply` included.
    let field_fn = if fields_of.is_empty() {
        quote!()
    } else {
        quote! {
            fn field_of(action: &Self::Action) -> ::core::option::Option<&'static str> {
                #[allow(unreachable_patterns, reason = "every other operation writes more than one field")]
                match &action.operation {
                    #(#fields_of)*
                    _ => ::core::option::Option::None,
                }
            }
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
        #[derive(Clone, Debug, #krate::__private::serde::Serialize)]
        #[serde(crate = #serde_path, tag = "action", content = "args")]
        pub enum #action_enum {
            #(#variants,)*
            #seed_variant
            /// A field edit: a path into the record and an operation.
            #[serde(rename = "field-edit")]
            FieldEdit(#krate::Edit<#self_ty>),
        }

        // Decoded through a JSON value by its action name: each verb's
        // parameters are decoded from one deserializer, not one per
        // deserializer the action meets.
        impl<'de> #krate::__private::serde::Deserialize<'de> for #action_enum {
            fn deserialize<D: #krate::__private::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                let value = <#krate::__private::serde_json::Value as #krate::__private::serde::Deserialize>::deserialize(deserializer)?;
                match Self::from_tagged(value) {
                    ::core::result::Result::Ok(action) => ::core::result::Result::Ok(action),
                    ::core::result::Result::Err(error) => ::core::result::Result::Err(
                        <D::Error as #krate::__private::serde::de::Error>::custom(error),
                    ),
                }
            }
        }

        impl #action_enum {
            /// The action in its JSON form, `{"action": .., "args": ..}`.
            #[inline(never)]
            fn from_tagged(
                value: #krate::__private::serde_json::Value,
            ) -> ::core::result::Result<Self, #krate::__private::serde_json::Error> {
                let (action, args) = #krate::__private::tagged(value)?;
                match action.as_str() {
                    #(#decode_arms)*
                    #seed_decode
                    "field-edit" => #krate::__private::serde::Deserialize::deserialize(args).map(Self::FieldEdit),
                    other => ::core::result::Result::Err(
                        <#krate::__private::serde_json::Error as #krate::__private::serde::de::Error>::unknown_variant(
                            other,
                            &[#(#verb_ids,)* "field-edit"],
                        ),
                    ),
                }
            }
        }

        impl #krate::Operation for #action_enum {
            type Record = #self_ty;
            const KIND: &'static str = #kind;
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
                    _ => #krate::ActionKind::#default_kind,
                }
            }
            fn field_edit(edit: #krate::Edit<#self_ty>) -> ::core::option::Option<Self> {
                ::core::option::Option::Some(Self::FieldEdit(edit))
            }
            #queue_fns
            #item_fn
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
            fn snapshot(record: &#self_ty) -> ::core::option::Option<#self_ty> {
                // A copy where the record is `Clone`, none where it is not.
                #[allow(unused_imports, reason = "only one of the two snapshot traits applies to a given record")]
                use #krate::__private::{SnapshotByClone as _, SnapshotUnavailable as _};
                (&&#krate::__private::Snapshot(record)).snapshot()
            }
            #risk
            #change_kind
            #incoming_fn
            #irreversible_fn
            #compensation_fns

            fn writes(&self) -> #krate::footprint::Writes {
                #[allow(unreachable_patterns, reason = "a field edit writes its path's unit")]
                match self {
                    #(#writes_arms)*
                    _ => #krate::footprint::Writes::record(),
                }
            }
        }

        impl #self_ty {
            #(#consts)*
            #compensation_inherent
            #queue_inherent
        }

        impl #krate::Resource for #self_ty {
            const KIND: &'static str = #kind;
            const LABEL: &'static str = #label;
            const RECORD_TABLE: ::core::option::Option<&'static #krate::table::ModelTable> =
                ::core::option::Option::Some(<Self as #krate::Model>::TABLE);
            type State = #self_ty;
            type Action = #krate::Stamped<#action_enum>;

            /// Its actions, and the undo of a change too large to record
            /// its restoring edits.
            fn register(registry: &mut #krate::__private::ActionRegistry) {
                registry.register::<Self::Action>(<Self as #krate::Resource>::ACTION_TYPE);
                registry.register::<#krate::Reverting<#action_enum>>(
                    #krate::reverting_type(<#action_enum as #krate::Operation>::KIND),
                );
            }

            fn read(doc: &#krate::Doc) -> ::core::result::Result<Self, #krate::ModelError> {
                #krate::evolution::check_version::<Self>(doc)?;
                <Self as #krate::Fields>::read_fields(doc, &#krate::__private::ROOT)
            }

            fn copy_state(state: &Self) -> ::core::option::Option<Self> {
                #[allow(unused_imports, reason = "only one of the two snapshot traits applies to a given record")]
                use #krate::__private::{SnapshotByClone as _, SnapshotUnavailable as _};
                (&&#krate::__private::Snapshot(state)).snapshot()
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

            /// A record's publication is a function of its state.
            fn publication_cache_epoch(_state: &Self) -> ::core::option::Option<::std::string::String> {
                ::core::option::Option::Some(::std::string::String::new())
            }

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

            fn continuable(action: &Self::Action) -> bool {
                match &action.operation {
                    #action_enum::FieldEdit(edit) => match edit.op() {
                        #krate::EditOp::Rich { rich } => {
                            <Self as #krate::Model>::op_continuable(edit.path(), rich).unwrap_or(true)
                        }
                        _ => true,
                    },
                    // A verb is a step of its own: nothing continues it.
                    _ => false,
                }
            }

            fn lands(state: &Self, action: &Self::Action) -> ::core::option::Option<#krate::Place> {
                #lands_first
                match &action.operation {
                    #action_enum::FieldEdit(edit) => match edit.op() {
                        #krate::EditOp::Rich { rich } => {
                            #krate::Model::op_lands(state, edit.path(), rich)
                        }
                        _ => ::core::option::Option::None,
                    },
                    _ => ::core::option::Option::None,
                }
            }

            fn op_schemas() -> ::std::vec::Vec<(::std::string::String, #krate::__private::serde_json::Value)> {
                let mut out = ::std::vec::Vec::new();
                <Self as #krate::Model>::op_schemas("", &mut out);
                out
            }

            #seed_fn
            #version
            #load
            #locate
            #availability
            #read_content
            #facts
            #document_child
            #views
            #field_fn

            fn children(state: &Self) -> ::std::vec::Vec<#krate::__private::Resource> {
                let _ = state;
                #children
            }

            #offer_bound
            #children_parts
            #item_effects
            #item_views
            #answer

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

            fn state_json(
                state: &Self,
            ) -> ::core::option::Option<#krate::__private::serde_json::Value> {
                ::core::option::Option::Some(<Self as #krate::Model>::to_json(state))
            }

            fn action_schema() -> ::core::option::Option<#krate::__private::serde_json::Value> {
                ::core::option::Option::Some(#krate::schema::record_action_schema::<Self>())
            }
        }
    })
}
