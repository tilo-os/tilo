#![warn(future_incompatible, rust_2018_compatibility, rust_2018_idioms, unused)]
#![warn(clippy::cargo, clippy::pedantic)]
#![cfg_attr(feature = "strict", deny(warnings))]

use crate::visit::YieldReplace;
use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro_hack::proc_macro_hack;
use quote::quote;
use syn::{
    parse_macro_input, parse_str, spanned::Spanned, visit_mut::VisitMut, ExprBlock, FnArg, ItemFn,
    Type,
};

mod visit;

fn call_site_error(message: &str) -> TokenStream {
    syn::Error::new(Span::call_site(), message)
        .to_compile_error()
        .into()
}

#[proc_macro_attribute]
pub fn stack_producer_fn(args: TokenStream, input: TokenStream) -> TokenStream {
    let a = args.clone();
    let _ = parse_macro_input!(a as Type);
    let mut function = parse_macro_input!(input as ItemFn);

    let co_arg = format!("{}{}>", stack::CO_ARG_FN, args);
    if let Err(err) = add_coroutine_arg(&mut function, &co_arg) {
        return err.to_compile_error().into();
    }

    YieldReplace.visit_item_fn_mut(&mut function);
    quote! { #function }.into()
}

#[proc_macro_hack]
pub fn stack_producer(input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as ExprBlock);
    YieldReplace.visit_expr_block_mut(&mut input);
    let arg = match parse_str::<FnArg>(stack::CO_ARG) {
        Ok(FnArg::Typed(x)) => x,
        _ => return call_site_error("string Pat parse failed Co<...>"),
    };
    quote! { |#arg| async move #input }.into()
}

#[proc_macro_attribute]
pub fn sync_producer_fn(args: TokenStream, input: TokenStream) -> TokenStream {
    let a = args.clone();
    let _ = parse_macro_input!(a as Type);
    let mut function = parse_macro_input!(input as ItemFn);

    let co_arg = format!("{}{}>", sync::CO_ARG_FN, args);
    if let Err(err) = add_coroutine_arg(&mut function, &co_arg) {
        return err.to_compile_error().into();
    }

    YieldReplace.visit_item_fn_mut(&mut function);
    quote! { #function }.into()
}

#[proc_macro_hack]
pub fn sync_producer(input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as ExprBlock);
    YieldReplace.visit_expr_block_mut(&mut input);
    let arg = match parse_str::<FnArg>(sync::CO_ARG) {
        Ok(FnArg::Typed(x)) => x,
        _ => return call_site_error("string Pat parse failed Co<...>"),
    };
    quote! { |#arg| async move #input }.into()
}

#[proc_macro_attribute]
pub fn rc_producer_fn(args: TokenStream, input: TokenStream) -> TokenStream {
    let a = args.clone();
    let _ = parse_macro_input!(a as Type);
    let mut function = parse_macro_input!(input as ItemFn);

    let co_arg = format!("{}{}>", rc::CO_ARG_FN, args);
    if let Err(err) = add_coroutine_arg(&mut function, &co_arg) {
        return err.to_compile_error().into();
    }

    YieldReplace.visit_item_fn_mut(&mut function);
    quote! { #function }.into()
}

#[proc_macro_hack]
pub fn rc_producer(input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as ExprBlock);
    YieldReplace.visit_expr_block_mut(&mut input);
    let arg = match parse_str::<FnArg>(rc::CO_ARG) {
        Ok(FnArg::Typed(x)) => x,
        _ => return call_site_error("string Pat parse failed Co<...>"),
    };
    quote! { |#arg| async move #input }.into()
}

mod stack {
    pub(crate) const CO_ARG_FN: &str = "__private_co_arg__: ::genawaiter::stack::Co<'_, ";
    pub(crate) const CO_ARG: &str = "__private_co_arg__: ::genawaiter::stack::Co<'_, _, _>";
}

mod sync {
    pub(crate) const CO_ARG_FN: &str = "__private_co_arg__: ::genawaiter::sync::Co<";
    pub(crate) const CO_ARG: &str = "__private_co_arg__: ::genawaiter::sync::Co<_, _>";
}

mod rc {
    pub(crate) const CO_ARG_FN: &str = "__private_co_arg__: ::genawaiter::rc::Co<";
    pub(crate) const CO_ARG: &str = "__private_co_arg__: ::genawaiter::rc::Co<_, _>";
}

fn add_coroutine_arg(func: &mut ItemFn, co_ty: &str) -> syn::Result<()> {
    let co_arg_found = func.sig.inputs.iter().any(|input| match input {
        FnArg::Receiver(_) => false,
        FnArg::Typed(arg) => match &*arg.ty {
            Type::Path(ty) => ty.path.segments.iter().any(|seg| seg.ident == "Co"),
            _ => false,
        },
    });

    if co_arg_found {
        return Err(syn::Error::new(
            func.sig.span(),
            "A generator producer cannot accept any arguments. Instead, consider using a closure and capturing the values you need.",
        ));
    }

    let co_arg = parse_str::<FnArg>(co_ty).map_err(|err| {
        syn::Error::new(
            Span::call_site(),
            format!("invalid type for Co yield {}", err),
        )
    })?;
    func.sig.inputs.push_value(co_arg);
    Ok(())
}
