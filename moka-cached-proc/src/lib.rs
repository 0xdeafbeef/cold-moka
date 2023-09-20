use proc_macro::TokenStream;

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
    name: Option<String>,
    #[darling(default)]
    size: Option<usize>,
    ttl: Option<u64>,
    #[darling(default)]
    key: Option<String>,

    #[darling(default)]
    convert: Option<String>,

    #[darling(default, rename = "type")]
    cache_type: Option<String>,
    #[darling(default, rename = "create")]
    cache_create: Option<String>,
}

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

    let input_names = get_input_names(&inputs);

    let ty_depths_info: Vec<u8> = input_names.iter().map(|x| x.1).collect();
    let input_names = input_names.iter().map(|x| x.0.clone()).collect();

    let input_tys = get_input_types(&inputs, &ty_depths_info);
    let function_call_args = get_wrapped_type_for_function_call(&inputs);

    // println!("input_tys: {:?}", param_names(&inputs));

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let return_ty = return_fallible_ty(&output);

    // pull out the output type

    let cache_value_ty = find_value_type(return_ty, &output, output_ty);

    // make the cache identifier
    let cache_ident = match args.name {
        Some(ref name) => Ident::new(name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    let (cache_key_ty, key_convert_block) = make_cache_key_type(
        &args.key,
        &args.convert,
        &args.cache_type,
        input_tys,
        &input_names,
    );

    let size = args.size.unwrap_or(1000);

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
        function_call_args,
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
            // static cache
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
                let result = #cache_ident.get_with_by_ref(&key, || #no_cache_fn_ident(#(#input_names),*));
                match result {
                    Ok(v) => v,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        (RetTurnTy::Result, true) => {
            quote! {
                let result = #cache_ident.get_with_by_ref(&key, #no_cache_fn_ident(#(#input_names),*)).await;
                match result {
                    Ok(v) => v,
                    Err(e) => Err(e.into()),
                }
            }
        }
        (RetTurnTy::Option, false) => {
            quote! {
                #cache_ident.get_with_by_ref(&key, #no_cache_fn_ident(#(#input_names),*))
            }
        }
        (RetTurnTy::Option, true) => {
            quote! {
                #cache_ident.get_with_by_ref(&key, #no_cache_fn_ident(#(#input_names),*)).await
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
