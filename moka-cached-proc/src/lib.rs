#![warn(
    clippy::all,
    clippy::dbg_macro,
    clippy::todo,
    clippy::empty_enum,
    clippy::enum_glob_use,
    clippy::mem_forget,
    clippy::unused_self,
    clippy::filter_map_next,
    clippy::needless_continue,
    clippy::needless_borrow,
    clippy::match_wildcard_for_single_variants,
    clippy::if_let_mutex,
    clippy::mismatched_target_os,
    clippy::await_holding_lock,
    clippy::match_on_vec_items,
    clippy::imprecise_flops,
    clippy::suboptimal_flops,
    clippy::lossy_float_literal,
    clippy::rest_pat_in_fully_bound_structs,
    clippy::fn_params_excessive_bools,
    clippy::exit,
    clippy::inefficient_to_string,
    clippy::linkedlist,
    clippy::macro_use_imports,
    clippy::option_option,
    clippy::verbose_file_reads,
    clippy::unnested_or_patterns,
    clippy::str_to_string,
    rust_2018_idioms,
    future_incompatible,
    nonstandard_style,
    missing_debug_implementations,
    missing_docs
)]

use proc_macro::TokenStream;
use std::collections::HashSet;

use darling::ast::NestedMeta;
use darling::FromMeta;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Ident, ItemFn, ReturnType};

use crate::helpers::*;

mod helpers;

#[derive(FromMeta)]
struct MacroArgs {
    #[darling(default)]
    size: Option<usize>,
    ttl: Option<u64>,
    #[darling(default)]
    // list of input names to use for the cache key
    key: Option<String>,

    #[darling(default)]
    convert: Option<String>,

    #[darling(default, rename = "type")]
    cache_type: Option<String>,
    #[darling(default, rename = "create")]
    cache_create: Option<String>,
}

/// ```ignore
/// use cold_moka::moka::sync::Cache;
/// use cold_moka::cached;
/// use cold_moka::once_cell::sync::Lazy;
///
///
/// #[cached(ttl = 100, size = 100)]
/// fn foo(bar: i32) -> i32 {
///     bar + 1
/// }
///
/// fn foo(bar: i32) -> i32 {
///     static FOO: Lazy<Cache<i32, i32>> = Lazy::new(|| {
///         Cache::builder()
///             .max_capacity(100)
///             .time_to_live(std::time::Duration::from_secs(100))
///             .build()
///     });
///     
///     fn foo_inner(bar: i32) -> i32 {
///         bar + 1
///     }
///
///     FOO.get_with_by_ref(&bar, || bar + 1)
/// }
/// ```
///
/// async functions will use `moka::future::Cache` instead of `moka::sync::Cache`
///
/// ```ignore
/// use cold_moka::moka::future::Cache;
/// use cold_moka::cached;
/// use cold_moka::once_cell::sync::Lazy;
///
/// #[cached(ttl = 100, size = 100)]
/// async fn bar(arg1: i32) ->String{
///   arg1.to_string()
/// }
///
/// // becomes
/// async fn bar(arg1: i32) ->String{
///  static BAR: Lazy<Cache<i32, String>> = Lazy::new(|| {
///        Cache::builder()
///           .max_capacity(100)
///          .time_to_live(std::time::Duration::from_secs(100))
///         .build()
///   });
///  BAR.get_with_by_ref(&arg1, || arg1.to_string()).await
/// }
/// ```
///
/// for functions with multiple arguments, you can specify which arguments to use for the cache key
///
/// ```rust
/// use cold_moka::cached;
///
/// struct Context;
///
/// #[cached(key = "arg1, arg2")]
/// async fn frobnicate(ctx: Context,arg1:i32, arg2: i32) -> Result<i32, String> {
///     Ok(arg1 + arg2)
/// }
/// ```
/// functions returning `Result` or `Option` will use `try_get_with_by_ref` and `optional_get_with_by_ref` respectively  
///
/// ```rust
/// use cold_moka::cached;
/// struct Context;
///
/// struct Wrapper<T>(T);
///
/// #[cached(key = "arg1,str")]
/// async fn frobnicate(
///     _ctx: Context,
///     Wrapper(arg1): Wrapper<i32> ,
///     str: String
/// ) -> Result<String, String> {
///     Ok(format!("{}{}", arg1, str))
/// }
/// ```
#[proc_macro_attribute]
pub fn cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };
    let args = match MacroArgs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };
    let input = parse_macro_input!(input as ItemFn);

    // pull out the parts of the input
    let attributes = input.attrs;
    let visibility = input.vis;
    let signature = input.sig;
    let body = input.block;

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let is_async = signature.asyncness.is_some();

    let filter_args_by: Option<HashSet<String>> = args.key.as_ref().map(|x| {
        x.split(',')
            .map(|x| x.trim().to_owned())
            .collect::<HashSet<String>>()
    });

    let input_names_with_depth: Vec<_> = get_input_names(&inputs).collect();
    let ty_depths_info: Vec<u8> = input_names_with_depth.iter().map(|x| x.1).collect();
    let input_tys = get_input_types(&inputs, &ty_depths_info);
    let input_names: Vec<_> = input_names_with_depth
        .into_iter()
        .map(|x| x.0.clone())
        .collect();

    let cache_key_type_indexes: HashSet<_> = input_names
        .iter()
        .enumerate()
        .filter_map(|(idx, ident)| {
            if let Some(filter) = &filter_args_by {
                filter.contains(ident.to_string().trim()).then_some(idx)
            } else {
                Some(idx)
            }
        })
        .collect();

    let inner_function_call_args = get_wrapped_type_for_function_call(&inputs);

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let return_ty = return_fallible_type(&output);
    let cache_value_ty = find_value_type(return_ty, &output, output_ty);
    let cache_ident = Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span());

    let (cache_key_ty, key_convert_block) = make_cache_key_type(
        &cache_key_type_indexes,
        &args.convert,
        &args.cache_type,
        input_tys,
        &input_names,
    );

    let size = if inner_function_call_args.is_empty() {
        args.size.unwrap_or(1) // () is the only possible input
    } else {
        args.size.unwrap_or(1000)
    };

    // make the cache type and create statement
    let (cache_ty, mut cache_create) =
        cache_creation_statement(&args, is_async, cache_value_ty, cache_key_ty, size as u64);
    if let Some(create) = args.cache_create {
        cache_create = quote! {#create};
    }

    let no_cache_fn_ident = Ident::new(&format!("{}_inner", fn_ident), fn_ident.span());
    let cache_type = quote! {
        static #cache_ident: ::cold_moka::once_cell::sync::Lazy<#cache_ty> = ::cold_moka::once_cell::sync::Lazy::new(|| #cache_create);
    };

    let function_no_cache = if is_async {
        quote! {
            async fn #no_cache_fn_ident(#inputs) #output #body
        }
    } else {
        quote! {
            fn #no_cache_fn_ident(#inputs) #output #body
        }
    };

    let function_call = inner_function_call(
        inner_function_call_args,
        return_ty,
        &cache_ident,
        no_cache_fn_ident,
        is_async,
    );

    let signature = get_mut_signature(signature);
    let expanded = quote!(
        #(#attributes)*
        #visibility
        // original function signature
        #signature
        {
            // inner function
            #function_no_cache
            // cache creation
            #cache_type
            let key = #key_convert_block;
            // call to inner function
            #function_call
        }
    );

    expanded.into()
}

fn inner_function_call(
    input_names: Vec<TokenStream2>,
    return_ty: RetTurnTy,
    cache_ident: &Ident,
    no_cache_fn_ident: Ident,
    is_async: bool,
) -> TokenStream2 {
    match (return_ty, is_async) {
        (RetTurnTy::Bare, false) => {
            quote! {
                #cache_ident.get_with_by_ref(&key, || #no_cache_fn_ident(#(#input_names),*))
            }
        }
        (RetTurnTy::Bare, true) => {
            quote! {
                #cache_ident.get_with_by_ref(&key,  #no_cache_fn_ident(#(#input_names),*)).await
            }
        }
        (RetTurnTy::Result, false) => {
            quote! {
                let result = #cache_ident.try_get_with_by_ref(&key, || #no_cache_fn_ident(#(#input_names),*));
                match result {
                    Ok(v) => Ok(v),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        (RetTurnTy::Result, true) => {
            quote! {
                let result = #cache_ident.try_get_with_by_ref(&key, #no_cache_fn_ident(#(#input_names),*)).await;
                match result {
                    Ok(v) => Ok(v),
                    Err(e) => Err(e.into()),
                }
            }
        }
        (RetTurnTy::Option, false) => {
            quote! {
                #cache_ident.optionally_get_with_by_ref(&key, #no_cache_fn_ident(#(#input_names),*))
            }
        }
        (RetTurnTy::Option, true) => {
            quote! {
                #cache_ident.optionally_get_with_by_ref(&key, #no_cache_fn_ident(#(#input_names),*)).await
            }
        }
    }
}

fn cache_creation_statement(
    args: &MacroArgs,
    is_async: bool,
    cache_value_ty: TokenStream2,
    cache_key_ty: TokenStream2,
    size: u64,
) -> (TokenStream2, TokenStream2) {
    let (cache_ty, cache_create) = match (args.ttl, is_async) {
        (Some(ttl), true) => {
            let cache_ty = quote! {
                ::cold_moka::moka::future::Cache<#cache_key_ty, #cache_value_ty>
            };

            let create = quote! {
                ::cold_moka::moka::future::Cache::builder().max_capacity(#size).time_to_live(::std::time::Duration::from_secs(#ttl)).build()
            };
            (cache_ty, create)
        }
        (None, true) => {
            let cache_ty = quote! {
                ::cold_moka::moka::future::Cache<#cache_key_ty, #cache_value_ty>
            };
            let create = quote! {
                ::cold_moka::moka::future::Cache::builder().max_capacity(#size).build()
            };
            (cache_ty, create)
        }
        (Some(ttl), false) => {
            let cache_ty = quote! {
                ::cold_moka::moka::sync::Cache<#cache_key_ty, #cache_value_ty>
            };
            let create = quote! {
               ::cold_moka::moka::sync::Cache::builder().max_capacity(#size).time_to_live(::std::time::Duration::from_secs(#ttl)).build()
            };
            (cache_ty, create)
        }
        (None, false) => {
            let cache_ty = quote! {
                ::cold_moka::moka::sync::Cache<#cache_key_ty, #cache_value_ty>
            };
            let create = quote! {
                ::cold_moka::moka::sync::Cache::builder().max_capacity(#size).build()
            };
            (cache_ty, create)
        }
    };
    (cache_ty, cache_create)
}

#[cfg(test)]
mod test {
    #[test]
    pub fn pass() {
        macrotest::expand("tests/*.rs");
    }
}
