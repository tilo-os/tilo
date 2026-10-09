//! `#[derive(Effect)]`: a typed effect vocabulary from an enum.

use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields};

use crate::{doc_lines, humanize, krate, param_type, snake_case};

/// A field's `#[param(…)]`: its description, after `secret,` when the
/// argument is a secret (an invite, a password) the host keeps out of logs,
/// history and confirmations, and then `choices = EXPR` when the values it
/// takes are a fixed list (any expression yielding strings, such as a
/// constant table). `place,` marks a place inside the resource
/// (`at: Option<Place>`): the effect acts there instead of at the invoker's
/// caret.
#[derive(Default)]
struct Param {
    secret: bool,
    place: bool,
    /// A documented struct whose fields are the effect's arguments
    /// (`#[param(flatten)]`, the effect's only field).
    flatten: bool,
    description: Option<String>,
    choices: Option<syn::Expr>,
}

impl syn::parse::Parse for Param {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut param = Self::default();
        if input.peek(syn::Ident) && !input.peek2(syn::Token![=]) {
            let flag: syn::Ident = input.parse()?;
            if flag == "flatten" {
                param.flatten = true;
            } else if flag == "secret" {
                param.secret = true;
            } else if flag == "place" {
                param.place = true;
            } else {
                return Err(syn::Error::new(
                    flag.span(),
                    "expected `secret`, `place` or `flatten`",
                ));
            }
            if input.is_empty() {
                return Ok(param);
            }
            input.parse::<syn::Token![,]>()?;
        }
        if input.peek(syn::LitStr) {
            param.description = Some(input.parse::<syn::LitStr>()?.value());
            if input.is_empty() {
                return Ok(param);
            }
            input.parse::<syn::Token![,]>()?;
        }
        let key: syn::Ident = input.parse()?;
        if key != "choices" {
            return Err(syn::Error::new(key.span(), "expected `choices = ...`"));
        }
        input.parse::<syn::Token![=]>()?;
        param.choices = Some(input.parse()?);
        Ok(param)
    }
}

struct EnumOptions {
    operation: String,
    device_only: bool,
}

fn enum_options(input: &DeriveInput) -> syn::Result<EnumOptions> {
    let mut operation = None;
    let mut device_only = false;
    for attr in input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("effect"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("operation") {
                operation = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                Ok(())
            } else if meta.path.is_ident("device_only") {
                device_only = true;
                Ok(())
            } else {
                Err(meta.error("expected `operation = \"...\"` or `device_only`"))
            }
        })?;
    }
    let operation = operation.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "name the effects' operation type: `#[effect(operation = \"app.effects\")]`",
        )
    })?;
    Ok(EnumOptions {
        operation,
        device_only,
    })
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per option the attribute spells out"
)]
struct VariantOptions {
    label: Option<String>,
    view: bool,
    row: bool,
    idempotent: bool,
    job: bool,
    cancellable: bool,
    returns: bool,
    content: bool,
    destructive: bool,
    privileged: bool,
    sensitive: bool,
    /// `device_only` (or `device_only = true`) marks this effect device-only;
    /// `device_only = false` exempts it from a vocabulary marked so.
    device_only: Option<bool>,
    risk: Option<syn::Path>,
    programmatic: bool,
    untrusted: bool,
    participant: bool,
    edit: bool,
    kind: Option<String>,
    /// The collection field whose items the effect is offered on, its first
    /// field bound to the item's id.
    item: Option<String>,
}

fn variant_options(attrs: &[syn::Attribute]) -> syn::Result<VariantOptions> {
    let mut options = VariantOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("effect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("label") {
                options.label = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("kind") {
                options.kind = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("view") {
                options.view = true;
            } else if meta.path.is_ident("row") {
                options.row = true;
            } else if meta.path.is_ident("idempotent") {
                options.idempotent = true;
            } else if meta.path.is_ident("job") {
                options.job = true;
            } else if meta.path.is_ident("cancellable") {
                options.cancellable = true;
            } else if meta.path.is_ident("returns") {
                options.returns = true;
            } else if meta.path.is_ident("content") {
                options.content = true;
            } else if meta.path.is_ident("destructive") {
                options.destructive = true;
            } else if meta.path.is_ident("privileged") {
                options.privileged = true;
            } else if meta.path.is_ident("sensitive") {
                options.sensitive = true;
            } else if meta.path.is_ident("device_only") {
                options.device_only = Some(if meta.input.peek(syn::Token![=]) {
                    meta.value()?.parse::<syn::LitBool>()?.value
                } else {
                    true
                });
            } else if meta.path.is_ident("risk") {
                options.risk = Some(meta.value()?.parse::<syn::LitStr>()?.parse::<syn::Path>()?);
            } else if meta.path.is_ident("programmatic") {
                options.programmatic = true;
            } else if meta.path.is_ident("untrusted") {
                options.untrusted = true;
            } else if meta.path.is_ident("participant") {
                options.participant = true;
            } else if meta.path.is_ident("edit") {
                options.edit = true;
            } else if meta.path.is_ident("item") {
                options.item = Some(meta.value()?.parse::<syn::Ident>()?.to_string());
            } else {
                return Err(meta.error(
                    "expected `label`, `view`, `row`, `job`, `cancellable`, `returns`, `content`, \
                     `destructive`, `privileged`, `sensitive`, `idempotent`, `device_only`, \
                     `risk`, `programmatic`, `untrusted`, `participant`, `edit`, `item = field` \
                     or `kind`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(options)
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear expansion per variant reads better than helpers threading token lists"
)]
pub(crate) fn derive(input: &DeriveInput) -> syn::Result<Tokens> {
    let krate = krate();
    let name = &input.ident;
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new(
            input.span(),
            "`Effect` is derived for an enum: one variant per effect",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "an effect vocabulary has no generic parameters",
        ));
    }
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "an effect vocabulary names at least one effect",
        ));
    }
    let EnumOptions {
        operation,
        device_only: all_device_only,
    } = enum_options(input)?;
    let any_arguments = data
        .variants
        .iter()
        .any(|variant| !matches!(variant.fields, Fields::Unit));

    let mut decls = Vec::new();
    let mut ids = Vec::new();
    let mut encodes = Vec::new();
    let mut decodes = Vec::new();
    let mut consts = Vec::new();
    for variant in &data.variants {
        let options = variant_options(&variant.attrs)?;
        let ident = &variant.ident;
        let id = snake_case(&ident.to_string()).replace('_', "-");
        let docs = doc_lines(&variant.attrs);
        let label = options.label.clone().unwrap_or_else(|| {
            docs.first()
                .map(|line| line.trim_end_matches('.').to_owned())
                .filter(|line| !line.is_empty())
                .unwrap_or_else(|| humanize(&snake_case(&ident.to_string())))
        });
        let description = docs
            .iter()
            .filter(|line| !line.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if options.view
            && (options.job
                || options.cancellable
                || options.destructive
                || options.privileged
                || options.sensitive)
        {
            return Err(syn::Error::new(
                variant.span(),
                "a `view` effect only changes what its caller sees: it answers at once and \
                 asks no confirmation (drop `job`, `cancellable`, `destructive`, `privileged` and `sensitive`)",
            ));
        }
        let action_kind = match options.kind.as_deref() {
            // An edit at the invoker's caret is an undoable change of the
            // record, whatever else it is.
            None if options.edit => quote!(Undoable),
            None if options.view => quote!(Read),
            None | Some("outside") => quote!(Outside),
            Some("read") => quote!(Read),
            Some("undoable") => quote!(Undoable),
            Some(other) => {
                return Err(syn::Error::new(
                    variant.span(),
                    format!("unknown kind `{other}`: `read`, `undoable` or `outside`"),
                ));
            }
        };

        let mut fields = Vec::new();
        match &variant.fields {
            Fields::Unit => {}
            Fields::Named(named) => {
                for field in &named.named {
                    let field_ident = field.ident.clone().ok_or_else(|| {
                        syn::Error::new(field.span(), "named field without a name")
                    })?;
                    let mut param = Param::default();
                    for attr in field
                        .attrs
                        .iter()
                        .filter(|attr| attr.path().is_ident("param"))
                    {
                        param = attr.parse_args::<Param>()?;
                    }
                    fields.push((field_ident, field.ty.clone(), param));
                }
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new(
                    variant.span(),
                    "an effect's arguments are named fields: `Save { draft: String }`",
                ));
            }
        }

        let flattened = fields.iter().any(|(_, _, described)| described.flatten);
        if flattened && fields.len() != 1 {
            return Err(syn::Error::new(
                variant.span(),
                "a `#[param(flatten)]` is the effect's only field: its fields are the arguments",
            ));
        }
        let params = fields.iter().map(|(field, ty, described)| {
            if described.flatten {
                return quote!(#krate::__private::flat_params(
                    &<#ty as #krate::JsonSchema>::json_schema()
                ));
            }
            let param = field.to_string();
            let description = described
                .description
                .clone()
                .unwrap_or_else(|| humanize(&param));
            let (param_ty, optional) = param_type(ty, false);
            let param_ty = if described.secret {
                quote!(#krate::__private::ParamType::Secret)
            } else {
                param_ty
            };
            let declared = if optional {
                quote!(#krate::__private::ActionParam::optional_owned(#param, #param_ty, #description))
            } else {
                quote!(#krate::__private::ActionParam::required_owned(#param, #param_ty, #description))
            };
            let declared = match &described.choices {
                Some(choices) => quote!(#declared.with_choices(#choices)),
                None => declared,
            };
            let declared = if described.place {
                quote!(#declared.as_place())
            } else {
                declared
            };
            quote!(::std::vec![#declared])
        });
        let answers = if options.job || options.cancellable {
            let cancellable = options.cancellable;
            let returns = options.returns;
            quote!(#krate::effect::Answers::Later { cancellable: #cancellable, returns: #returns })
        } else if options.content {
            quote!(#krate::effect::Answers::Content)
        } else if options.returns {
            quote!(#krate::effect::Answers::Value)
        } else {
            quote!(#krate::effect::Answers::Nothing)
        };
        let risk = match (options.destructive, options.privileged, options.sensitive) {
            (true, false, false) => quote!(Destructive),
            (false, true, false) => quote!(Privileged),
            (false, false, true) => quote!(Sensitive),
            (false, false, false) if options.view => quote!(None),
            (false, false, false) => quote!(Low),
            _ => {
                return Err(syn::Error::new(
                    variant.span(),
                    "an effect is one of `destructive`, `privileged` or `sensitive`",
                ));
            }
        };
        let idempotent = options.idempotent;
        let device_only = options.device_only.unwrap_or(all_device_only);
        let risk_of = if let Some(classify) = &options.risk {
            quote! {
                ::std::option::Option::Some(
                    (|args: &#krate::__private::ActionArgs| -> #krate::__private::RiskLevel {
                        match <Self as #krate::Effect>::decode(#id, &args.to_json()) {
                            ::std::result::Result::Ok(effect) => #classify(&effect),
                            ::std::result::Result::Err(_) => #krate::__private::RiskLevel::Destructive,
                        }
                    }) as fn(&#krate::__private::ActionArgs) -> #krate::__private::RiskLevel,
                )
            }
        } else {
            quote!(::std::option::Option::None)
        };
        let row = options.row;
        let programmatic = options.programmatic;
        let untrusted = options.untrusted;
        if options.participant && !options.view && !options.edit {
            return Err(syn::Error::new(
                variant.span(),
                "`participant` declares a view of the invoking participant: add `view`",
            ));
        }
        let participant = options.participant;
        let edits = options.edit;
        if options.item.is_some() && fields.is_empty() {
            return Err(syn::Error::new(
                variant.span(),
                "an effect offered on items binds its first field to the item: give it one",
            ));
        }
        let item = options.item.as_ref().map_or_else(
            || quote!(::core::option::Option::None),
            |field| quote!(::core::option::Option::Some(#field)),
        );
        let kind = if options.view || options.edit {
            quote!(View)
        } else {
            quote!(Effect)
        };
        decls.push(quote! {
            #krate::effect::EffectDecl {
                id: #id,
                label: #label,
                description: #description,
                params: {
                    let mut params: ::std::vec::Vec<#krate::__private::ActionParam> =
                        ::std::vec::Vec::new();
                    #(params.extend(#params);)*
                    params
                },
                kind: #krate::__private::OperationKind::#kind,
                row: #row,
                answers: #answers,
                risk: #krate::__private::RiskLevel::#risk,
                idempotent: #idempotent,
                action_kind: #krate::__private::ActionKind::#action_kind,
                device_only: #device_only,
                risk_of: #risk_of,
                programmatic: #programmatic,
                untrusted: #untrusted,
                participant: #participant,
                edits: #edits,
                item: #item,
            }
        });

        let field_idents: Vec<_> = fields.iter().map(|(field, _, _)| field.clone()).collect();
        let field_names: Vec<String> = field_idents.iter().map(ToString::to_string).collect();
        let pattern = if matches!(variant.fields, Fields::Unit) {
            quote!(Self::#ident)
        } else {
            quote!(Self::#ident { #(#field_idents,)* })
        };
        let ignore = if matches!(variant.fields, Fields::Unit) {
            quote!(Self::#ident)
        } else {
            quote!(Self::#ident { .. })
        };
        ids.push(quote!(#ignore => #id,));
        let (encode, build) = if let Some(field) = field_idents.first().filter(|_| flattened) {
            (
                quote! {
                    if let ::core::result::Result::Ok(#krate::__private::serde_json::Value::Object(fields)) =
                        #krate::__private::serde_json::to_value(#field)
                    {
                        // An absent option is left out, as an argument not given.
                        arguments.extend(fields.into_iter().filter(|(_, value)| !value.is_null()));
                    }
                },
                quote!(Self::#ident {
                    #field: #krate::__private::serde_json::from_value(
                        #krate::__private::serde_json::Value::Object(::core::clone::Clone::clone(arguments)),
                    )
                    .map_err(|error| ::std::string::ToString::to_string(&error))?,
                }),
            )
        } else if matches!(variant.fields, Fields::Unit) {
            (quote!(), quote!(Self::#ident))
        } else {
            (
                quote!(#(#krate::effect::put(&mut arguments, #field_names, #field_idents);)*),
                quote!(Self::#ident { #(#field_idents: #krate::effect::argument(arguments, #field_names)?,)* }),
            )
        };
        encodes.push(quote! {
            #pattern => {
                #encode
            }
        });
        decodes.push(quote!(#id => ::core::result::Result::Ok(#build),));

        let const_ident = format_ident!("{}", snake_case(&ident.to_string()).to_uppercase());
        let tys: Vec<&syn::Type> = fields.iter().map(|(_, ty, _)| ty).collect();
        let input_ty = match tys.as_slice() {
            [] => quote!(()),
            [single] => quote!(#single),
            many => quote!((#(#many,)*)),
        };
        let const_doc = format!("Bind a control to the `{id}` effect: {label}.");
        consts.push(quote! {
            #[doc = #const_doc]
            pub const #const_ident: #krate::ActionRef<#name, #input_ty> =
                #krate::ActionRef::new(#id, &[#(#field_names),*]);
        });
    }

    let arguments_binding = if any_arguments {
        quote!(arguments)
    } else {
        quote!(_arguments)
    };
    Ok(quote! {
        impl #krate::Effect for #name {
            const OPERATION: &'static str = #operation;

            fn declarations() -> ::std::vec::Vec<#krate::effect::EffectDecl> {
                ::std::vec![#(#decls),*]
            }

            fn id(&self) -> &'static str {
                match self {
                    #(#ids)*
                }
            }

            #[allow(unused_mut, reason = "a unit effect has no arguments to insert")]
            fn arguments(&self) -> #krate::__private::serde_json::Map<::std::string::String, #krate::__private::serde_json::Value> {
                let mut arguments = #krate::__private::serde_json::Map::new();
                match self {
                    #(#encodes)*
                }
                arguments
            }

            fn decode(
                id: &str,
                #arguments_binding: &#krate::__private::serde_json::Map<::std::string::String, #krate::__private::serde_json::Value>,
            ) -> ::core::result::Result<Self, ::std::string::String> {
                match id {
                    #(#decodes)*
                    other => ::core::result::Result::Err(::std::format!("there is no effect `{other}`")),
                }
            }
        }

        #[allow(dead_code, reason = "a control binds the effects it offers, not necessarily all")]
        impl #name {
            #(#consts)*
        }
    })
}
