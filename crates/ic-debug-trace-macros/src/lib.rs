//! `#[trace_method]` attribute macro.
//!
//! Wraps an `async fn` or `fn` so that on entry it emits a `MethodEntered`
//! event and on exit (including the Err branch) a `MethodExited`.
//!
//! The macro does **not** change the signature, so it composes with the
//! existing `#[update]` / `#[query]` macros from `ic_cdk_macros`. Apply
//! `#[trace_method]` *above* `#[update]` so the body is wrapped before the
//! CDK code-gens the Candid entry point.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, PatIdent};

/// Wraps a canister method so that:
///   1. the inbound `TraceHeader` (required as the **first** argument) is
///      adopted via `begin_trace` _before_ anything else runs;
///   2. a `MethodEntered` event is emitted on entry;
///   3. a `MethodExited` event is emitted on any exit path (Drop guard).
///
/// The macro does **not** change the signature, so it composes with
/// `#[update]` / `#[query]` from `ic_cdk_macros`. Put `#[trace_method]`
/// *above* `#[update]`.
#[proc_macro_attribute]
pub fn trace_method(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let ItemFn { attrs, vis, sig, block } = func;
    let method_name = sig.ident.to_string();
    let is_async = sig.asyncness.is_some();

    // Walk the typed arguments. The first typed arg is the `TraceHeader` —
    // we pull its ident out for `begin_trace`. Every remaining typed arg is
    // captured as `(stringify!(name), format!("{:?}", &name))` so the
    // timeline's ENTER row shows what the method was called with.
    let mut typed_idents: Vec<syn::Ident> = Vec::new();
    for arg in sig.inputs.iter() {
        if let FnArg::Typed(t) = arg {
            if let Pat::Ident(PatIdent { ident, .. }) = &*t.pat {
                typed_idents.push(ident.clone());
            }
        }
    }

    let (begin, arg_idents): (_, &[syn::Ident]) = match typed_idents.split_first() {
        Some((header, rest)) => (
            quote! { ::ic_debug_trace::begin_trace(#header); },
            rest,
        ),
        None => (quote! {}, &[][..]),
    };

    let arg_names: Vec<String> = arg_idents.iter().map(|i| i.to_string()).collect();

    let capture_args = quote! {
        let __args: ::std::vec::Vec<(::std::string::String, ::std::string::String)> =
            ::std::vec![
                #( (
                    ::std::string::String::from(#arg_names),
                    ::std::format!("{:?}", &#arg_idents),
                ) ),*
            ];
    };

    let body = if is_async {
        quote! {{
            #begin
            #capture_args
            ::ic_debug_trace::on_method_enter(#method_name, __args);
            let __guard = ::ic_debug_trace::MethodExitGuard;
            let __result = async move #block.await;
            ::core::mem::drop(__guard);
            __result
        }}
    } else {
        quote! {{
            #begin
            #capture_args
            ::ic_debug_trace::on_method_enter(#method_name, __args);
            let __guard = ::ic_debug_trace::MethodExitGuard;
            let __result = (|| #block)();
            ::core::mem::drop(__guard);
            __result
        }}
    };

    let expanded = quote! {
        #(#attrs)*
        #vis #sig #body
    };

    expanded.into()
}
