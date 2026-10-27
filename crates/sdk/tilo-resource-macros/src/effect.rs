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
/// caret; `place = "field"` names the collection whose items it takes.
#[derive(Default)]
struct Param {
    /// What the argument is beyond a value, by its leading flag.
    flag: Flag,
    /// The collection whose items the place names (`place = "blocks"`).
    place_in: Option<String>,
    description: Option<String>,
    choices: Option<syn::Expr>,
    /// The argument's name where it differs from the field's (`name =
    /// "pageSize"`): a contract its callers already speak.
    name: Option<String>,
}

/// A parameter's leading flag.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Flag {
    #[default]
    None,
    /// A secret the host keeps out of logs, history and confirmations.
    Secret,
    /// Content another action produced, carried as its hash; on a
    /// `Vec<String>`, a list of content, each item its hash.
    Content,
    /// A place inside the resource.
    Place,
    /// A documented struct whose fields are the effect's arguments (the
    /// effect's only field).
    Flatten,
}

impl syn::parse::Parse for Param {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut param = Self::default();
        if input.peek(syn::Ident) && !input.peek2(syn::Token![=]) {
            let flag: syn::Ident = input.parse()?;
            if flag == "flatten" {
                param.flag = Flag::Flatten;
            } else if flag == "secret" {
                param.flag = Flag::Secret;
            } else if flag == "place" {
                param.flag = Flag::Place;
            } else if flag == "content" {
                param.flag = Flag::Content;
            } else {
                return Err(syn::Error::new(
                    flag.span(),
                    "expected `secret`, `content`, `place` or `flatten`",
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
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            if key == "place" {
                param.flag = Flag::Place;
                param.place_in = Some(input.parse::<syn::LitStr>()?.value());
            } else if key == "name" {
                param.name = Some(input.parse::<syn::LitStr>()?.value());
            } else if key == "choices" {
                param.choices = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `choices = ...`, `place = \"field\"` or `name = \"argument\"`",
                ));
            }
            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(param)
    }
}

fn enum_operation(input: &DeriveInput) -> syn::Result<String> {
    let mut operation = None;
    for attr in input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("effect"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("operation") {
                operation = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                Ok(())
            } else {
                Err(meta.error("expected `operation = \"...\"`"))
            }
        })?;
    }
    operation.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "name the effects' operation type: `#[effect(operation = \"app.effects\")]`",
        )
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
    /// `own_device`: the effect's whole scope is the invoking person's own
    /// device, touching nothing shared and no one else.
    own_device: bool,
    /// `attended`: the effect needs its person attending (a chooser, a
    /// credential, something only they may see): anyone acting for them is
    /// always asked first.
    attended: bool,
    risk: Option<syn::Path>,
    /// `audience = "agents"` or `"people"`: who listings offer it to.
    audience: Option<syn::Ident>,
    untrusted: bool,
    /// `opens_path` declares the effect opens the file its `path` names;
    /// `opens_path = "csv,tsv"` limits it to those extensions.
    opens_path: Option<Vec<String>>,
    participant: bool,
    edit: bool,
    member: bool,
    kind: Option<String>,
    /// The collection field whose items the effect is offered on, its first
    /// field bound to the item's id.
    item: Option<String>,
    /// `offload(app = "…", entrypoint = "…", cpu_ms = N, memory_mb = N,
    /// output_mb = N, gpu)`: a content transform the system may run on the
    /// invoker's best-placed device as the package's catalogue entrypoint.
    offload: Option<OffloadOption>,
    /// `host_render`: a host answers it in a render service of its own,
    /// never in the record's confined app worker.
    host_render: bool,
    /// `tag = <expr>`, repeatable: further contract tags, each a
    /// `&'static str` constant.
    tags: Vec<syn::Expr>,
    /// `outside_with = "param"`: a read that reaches outside the record only
    /// when that parameter is given.
    outside_with: Option<String>,
}

/// An effect's `offload(...)` option, as written.
struct OffloadOption {
    app: String,
    entrypoint: String,
    gpu: bool,
    cpu_ms: u64,
    memory_mb: u64,
    output_mb: u64,
}

/// Parse `offload(app = "…", entrypoint = "…", cpu_ms = N, memory_mb = N,
/// output_mb = N, gpu)`; every key but `gpu` is required.
fn offload_option(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<OffloadOption> {
    let mut app = None;
    let mut entrypoint = None;
    let mut gpu = false;
    let mut cpu_ms = None;
    let mut memory_mb = None;
    let mut output_mb = None;
    let number = |inner: &syn::meta::ParseNestedMeta<'_>| -> syn::Result<u64> {
        inner.value()?.parse::<syn::LitInt>()?.base10_parse()
    };
    meta.parse_nested_meta(|inner| {
        if inner.path.is_ident("app") {
            app = Some(inner.value()?.parse::<syn::LitStr>()?.value());
        } else if inner.path.is_ident("entrypoint") {
            entrypoint = Some(inner.value()?.parse::<syn::LitStr>()?.value());
        } else if inner.path.is_ident("gpu") {
            gpu = true;
        } else if inner.path.is_ident("cpu_ms") {
            cpu_ms = Some(number(&inner)?);
        } else if inner.path.is_ident("memory_mb") {
            memory_mb = Some(number(&inner)?);
        } else if inner.path.is_ident("output_mb") {
            output_mb = Some(number(&inner)?);
        } else {
            return Err(inner.error(
                "expected `app`, `entrypoint`, `cpu_ms`, `memory_mb`, `output_mb` or `gpu`",
            ));
        }
        Ok(())
    })?;
    let missing = |key: &str| meta.error(format!("`offload(...)` needs `{key} = ...`"));
    Ok(OffloadOption {
        app: app.ok_or_else(|| missing("app"))?,
        entrypoint: entrypoint.ok_or_else(|| missing("entrypoint"))?,
        gpu,
        cpu_ms: cpu_ms.ok_or_else(|| missing("cpu_ms"))?,
        memory_mb: memory_mb.ok_or_else(|| missing("memory_mb"))?,
        output_mb: output_mb.ok_or_else(|| missing("output_mb"))?,
    })
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
            } else if meta.path.is_ident("own_device") {
                options.own_device = true;
            } else if meta.path.is_ident("attended") {
                options.attended = true;
            } else if meta.path.is_ident("risk") {
                options.risk = Some(meta.value()?.parse::<syn::LitStr>()?.parse::<syn::Path>()?);
            } else if meta.path.is_ident("audience") {
                options.audience = Some(crate::audience_option(&meta)?);
            } else if meta.path.is_ident("untrusted") {
                options.untrusted = true;
            } else if meta.path.is_ident("opens_path") {
                options.opens_path = Some(if meta.input.peek(syn::Token![=]) {
                    meta.value()?
                        .parse::<syn::LitStr>()?
                        .value()
                        .split(',')
                        .map(|extension| extension.trim().to_ascii_lowercase())
                        .filter(|extension| !extension.is_empty())
                        .collect()
                } else {
                    Vec::new()
                });
            } else if meta.path.is_ident("participant") {
                options.participant = true;
            } else if meta.path.is_ident("edit") {
                options.edit = true;
            } else if meta.path.is_ident("member") {
                options.member = true;
            } else if meta.path.is_ident("item") {
                options.item = Some(meta.value()?.parse::<syn::Ident>()?.to_string());
            } else if meta.path.is_ident("offload") {
                options.offload = Some(offload_option(&meta)?);
            } else if meta.path.is_ident("host_render") {
                options.host_render = true;
            } else if meta.path.is_ident("tag") {
                options.tags.push(meta.value()?.parse::<syn::Expr>()?);
            } else if meta.path.is_ident("outside_with") {
                options.outside_with = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else {
                return Err(meta.error(
                    "expected `label`, `view`, `row`, `job`, `cancellable`, `returns`, `content`, \
                     `destructive`, `privileged`, `sensitive`, `idempotent`, \
                     `own_device`, `attended`, `risk`, `audience`, `untrusted`, `opens_path`, `participant`, `edit`, `member`, \
                     `item = field`, `offload(...)`, `host_render`, `tag = ...`, `outside_with` or `kind`",
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
    let operation = enum_operation(input)?;
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

        let flattened = fields
            .iter()
            .any(|(_, _, described)| described.flag == Flag::Flatten);
        if flattened && fields.len() != 1 {
            return Err(syn::Error::new(
                variant.span(),
                "a `#[param(flatten)]` is the effect's only field: its fields are the arguments",
            ));
        }
        let params = fields.iter().map(|(field, ty, described)| {
            if described.flag == Flag::Flatten {
                return quote!(#krate::__private::flat_params(
                    &<#ty as #krate::JsonSchema>::json_schema()
                ));
            }
            let param = described.name.clone().unwrap_or_else(|| field.to_string());
            let description = described
                .description
                .clone()
                .unwrap_or_else(|| humanize(&param));
            let (param_ty, optional) = param_type(ty, false);
            let param_ty = if described.flag == Flag::Secret {
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
            let declared = if described.flag == Flag::Content {
                // A list of content (`Vec<String>`) names each item's hash.
                let list = crate::last_ident(ty).is_some_and(|(name, _)| name == "Vec");
                if list {
                    quote!(#declared.as_content_list())
                } else {
                    quote!(#declared.as_content())
                }
            } else {
                declared
            };
            let declared = match (&described.place_in, described.flag == Flag::Place) {
                (Some(field), _) => quote!(#declared.as_place_in(#field)),
                (None, true) => quote!(#declared.as_place()),
                (None, false) => declared,
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
        let own_device = options.own_device;
        let attended = options.attended;
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
        let audience = options
            .audience
            .clone()
            .unwrap_or_else(|| syn::Ident::new("Everyone", variant.span()));
        let untrusted = options.untrusted;
        let opens = options.opens_path.as_ref().map_or_else(
            || quote!(::core::option::Option::None),
            |extensions| quote!(::core::option::Option::Some(&[#(#extensions),*])),
        );
        if options.participant && !options.view && !options.edit {
            return Err(syn::Error::new(
                variant.span(),
                "`participant` declares a view of the invoking participant: add `view`",
            ));
        }
        let participant = options.participant;
        let edits = options.edit;
        let member = options.member;
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
        let reads = match options.kind.as_deref() {
            Some(kind) => kind == "read",
            None => options.view && !options.edit,
        };
        let offload = match &options.offload {
            None => quote!(::core::option::Option::None),
            Some(_) if !reads || !options.content => {
                return Err(syn::Error::new(
                    variant.span(),
                    "an `offload` effect is a content transform any device may run: declare it \
                     `content` and `kind = \"read\"`",
                ));
            }
            Some(OffloadOption {
                app,
                entrypoint,
                gpu,
                cpu_ms,
                memory_mb,
                output_mb,
            }) => {
                let mib = |megabytes: u64| megabytes.saturating_mul(1024 * 1024);
                let (memory_bytes, output_bytes) = (mib(*memory_mb), mib(*output_mb));
                quote!(::core::option::Option::Some(#krate::effect::EffectOffload {
                    app: #app,
                    entrypoint: #entrypoint,
                    gpu: #gpu,
                    cpu_time_ms: #cpu_ms,
                    memory_bytes: #memory_bytes,
                    output_bytes: #output_bytes,
                }))
            }
        };
        let kind = if options.view || options.edit {
            quote!(View)
        } else {
            quote!(Effect)
        };
        // Each one a constant, as the row it lands in is.
        let host_render = options
            .host_render
            .then(|| quote!(#krate::__private::HOST_RENDER_TAG))
            .into_iter();
        let declared = &options.tags;
        let tags = quote!(&[#(#host_render,)* #(#declared),*]);
        let outside_with = match &options.outside_with {
            None => quote!(::core::option::Option::None),
            Some(_) if !reads => {
                return Err(syn::Error::new(
                    variant.span(),
                    "`outside_with` names what makes a read reach outside: declare it \
                     `kind = \"read\"`",
                ));
            }
            Some(param) => quote!(::core::option::Option::Some(#param)),
        };
        let params: Vec<Tokens> = params.collect();
        let params_fn = if params.is_empty() {
            quote!(#krate::effect::no_params)
        } else {
            quote!(|| {
                let mut params: ::std::vec::Vec<#krate::__private::ActionParam> =
                    ::std::vec::Vec::new();
                #(params.extend(#params);)*
                params
            })
        };
        decls.push(quote! {
            #krate::effect::EffectRow {
                id: #id,
                label: #label,
                description: #description,
                params: #params_fn,
                kind: #krate::__private::OperationKind::#kind,
                row: #row,
                answers: #answers,
                risk: #krate::__private::RiskLevel::#risk,
                idempotent: #idempotent,
                action_kind: #krate::__private::ActionKind::#action_kind,
                own_device: #own_device,
                attended: #attended,
                risk_of: #risk_of,
                audience: #krate::__private::Audience::#audience,
                untrusted: #untrusted,
                opens: #opens,
                participant: #participant,
                edits: #edits,
                member: #member,
                item: #item,
                offload: #offload,
                tags: #tags,
                outside_with: #outside_with,
            }
        });

        let field_idents: Vec<_> = fields.iter().map(|(field, _, _)| field.clone()).collect();
        let field_names: Vec<String> = fields
            .iter()
            .map(|(field, _, described)| {
                described.name.clone().unwrap_or_else(|| field.to_string())
            })
            .collect();
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
    let row_count = decls.len();
    let rows = decls.into_iter().map(|row| name_self(row, name));
    Ok(quote! {
        impl #krate::Effect for #name {
            const OPERATION: &'static str = #operation;

            fn declarations() -> ::std::vec::Vec<#krate::effect::EffectDecl> {
                #krate::effect::EffectDecl::of_rows(Self::__effect_rows())
            }

            fn declaration(id: &str) -> ::core::option::Option<#krate::effect::EffectDecl> {
                #krate::effect::EffectDecl::find_row(Self::__effect_rows(), id)
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

            /// Every effect's declaration, read by one function for every
            /// vocabulary. A `static` rather than a `const`, so the table has
            /// one address and is never built at a use site: a large app's
            /// rows outgrow what may sit on the stack.
            #[doc(hidden)]
            pub fn __effect_rows() -> &'static [#krate::effect::EffectRow] {
                static ROWS: [#krate::effect::EffectRow; #row_count] = [#(#rows),*];
                &ROWS
            }
        }
    })
}

/// The rows live in a `static` inside a function, which cannot see the
/// impl's `Self`: every `Self` in a row (the derive's own or one an app wrote
/// in an attribute expression) names the enum instead. The derive rejects
/// generic enums, so the bare name is the whole type.
fn name_self(tokens: Tokens, name: &syn::Ident) -> Tokens {
    use proc_macro2::{Group, TokenTree};
    tokens
        .into_iter()
        .map(|tree| match tree {
            TokenTree::Ident(ident) if ident == "Self" => {
                TokenTree::Ident(syn::Ident::new(&name.to_string(), ident.span()))
            }
            TokenTree::Group(group) => {
                let mut renamed = Group::new(group.delimiter(), name_self(group.stream(), name));
                renamed.set_span(group.span());
                TokenTree::Group(renamed)
            }
            other => other,
        })
        .collect()
}
