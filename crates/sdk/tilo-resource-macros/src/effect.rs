//! `#[derive(Effect)]`: a typed effect vocabulary from an enum.

use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields};

use crate::{doc_lines, humanize, krate, param_type, snake_case};

struct EnumOptions {
    operation: String,
}

fn enum_options(input: &DeriveInput) -> syn::Result<EnumOptions> {
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
    let operation = operation.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "name the effects' operation type: `#[effect(operation = \"app.effects\")]`",
        )
    })?;
    Ok(EnumOptions { operation })
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per option the attribute spells out"
)]
struct VariantOptions {
    label: Option<String>,
    job: bool,
    cancellable: bool,
    returns: bool,
    destructive: bool,
    permission: Option<String>,
}

fn variant_options(attrs: &[syn::Attribute]) -> syn::Result<VariantOptions> {
    let mut options = VariantOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("effect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("label") {
                options.label = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("permission") {
                options.permission = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("job") {
                options.job = true;
            } else if meta.path.is_ident("cancellable") {
                options.cancellable = true;
            } else if meta.path.is_ident("returns") {
                options.returns = true;
            } else if meta.path.is_ident("destructive") {
                options.destructive = true;
            } else {
                return Err(meta.error(
                    "expected `label`, `job`, `cancellable`, `returns`, `destructive` or `permission`",
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
    let operation = enum_options(input)?.operation;
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
        let permission = match options.permission.as_deref() {
            None | Some("write") => quote!(Write),
            Some("none") => quote!(None),
            Some("read") => quote!(Read),
            Some("comment") => quote!(Comment),
            Some("invite") => quote!(Invite),
            Some("admin") => quote!(Admin),
            Some(other) => {
                return Err(syn::Error::new(
                    variant.span(),
                    format!("unknown permission `{other}`"),
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
                    let mut described = None;
                    for attr in field
                        .attrs
                        .iter()
                        .filter(|attr| attr.path().is_ident("param"))
                    {
                        described = Some(attr.parse_args::<syn::LitStr>()?.value());
                    }
                    fields.push((field_ident, field.ty.clone(), described));
                }
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new(
                    variant.span(),
                    "an effect's arguments are named fields: `Save { draft: String }`",
                ));
            }
        }

        let params = fields.iter().map(|(field, ty, described)| {
            let param = field.to_string();
            let description = described.clone().unwrap_or_else(|| humanize(&param));
            let (param_ty, optional) = param_type(ty);
            if optional {
                quote!(#krate::__private::ActionParam::optional_owned(#param, #param_ty, #description))
            } else {
                quote!(#krate::__private::ActionParam::required_owned(#param, #param_ty, #description))
            }
        });
        let answers = if options.job || options.cancellable {
            let cancellable = options.cancellable;
            quote!(#krate::effect::Answers::Later { cancellable: #cancellable })
        } else if options.returns {
            quote!(#krate::effect::Answers::Value)
        } else {
            quote!(#krate::effect::Answers::Nothing)
        };
        let destructive = options.destructive;
        decls.push(quote! {
            #krate::effect::EffectDecl {
                id: #id,
                label: #label,
                description: #description,
                params: ::std::vec![#(#params),*],
                answers: #answers,
                destructive: #destructive,
                permission: #krate::__private::PermissionRequirement::#permission,
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
        encodes.push(quote! {
            #pattern => {
                #(#krate::effect::put(&mut arguments, #field_names, #field_idents);)*
            }
        });
        let build = if matches!(variant.fields, Fields::Unit) {
            quote!(Self::#ident)
        } else {
            quote!(Self::#ident { #(#field_idents: #krate::effect::argument(arguments, #field_names)?,)* })
        };
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
