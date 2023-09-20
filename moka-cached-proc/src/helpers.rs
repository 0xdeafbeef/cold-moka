use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use std::iter;
use std::ops::Deref;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{
    parse_str, Block, FieldPat, FnArg, GenericArgument, Pat, PatIdent, PatReference, PatStruct,
    PatTuple, PatTupleStruct, PatType, PathArguments, ReturnType, Signature, Type,
};

// if you define arguments as mutable, e.g.
// #[cached]
// fn mutable_args(mut a: i32, mut b: i32) -> (i32, i32) {
//     a += 1;
//     b += 1;
//     (a, b)
// }
// then we want the `mut` keywords present on the "inner" function
// that wraps your actual block of code.
// If the `mut`s are also on the outer method, then you'll
// get compiler warnings about your arguments not needing to be `mut`
// when they really do need to be.
pub(super) fn get_mut_signature(signature: Signature) -> Signature {
    let mut signature_no_muts = signature;
    let mut sig_inputs = Punctuated::new();
    for inp in &signature_no_muts.inputs {
        let item = match inp {
            FnArg::Receiver(_) => inp.clone(),
            FnArg::Typed(pat_type) => {
                let mut pt = pat_type.clone();
                let pat = strip_mut_from_pat(pat_type);
                pt.pat = pat;
                FnArg::Typed(pt)
            }
        };
        sig_inputs.push(item);
    }
    signature_no_muts.inputs = sig_inputs;
    signature_no_muts
}

pub(super) fn strip_mut_from_pat(pat_type: &PatType) -> Box<Pat> {
    match &pat_type.pat.deref() {
        Pat::Ident(pat_ident) => {
            if pat_ident.mutability.is_some() {
                let mut p = pat_ident.clone();
                p.mutability = None;
                Box::new(Pat::Ident(p))
            } else {
                Box::new(Pat::Ident(pat_ident.clone()))
            }
        }
        _ => pat_type.pat.clone(),
    }
}

pub fn param_names(pat: Pat, depth: u8) -> Box<dyn Iterator<Item = (Ident, u8)>> {
    match pat {
        Pat::Ident(PatIdent { ident, .. }) => Box::new(iter::once((ident, depth))),
        Pat::Reference(PatReference { pat, .. }) => param_names(*pat, depth + 1),
        // We can't get the concrete type of fields in the struct/tuple
        // patterns by using `syn`. e.g. `fn foo(Foo { x, y }: Foo) {}`.
        // Therefore, the struct/tuple patterns in the arguments will just
        // always be recorded as `RecordType::Debug`.
        Pat::Struct(PatStruct { fields, .. }) => Box::new(
            fields
                .into_iter()
                .flat_map(move |FieldPat { pat, .. }| param_names(*pat, depth + 1)),
        ),
        Pat::Tuple(PatTuple { elems, .. }) => Box::new(
            elems
                .into_iter()
                .flat_map(move |p| param_names(p, depth + 1)),
        ),
        Pat::TupleStruct(PatTupleStruct { elems, .. }) => Box::new(
            elems
                .into_iter()
                .flat_map(move |p| param_names(p, depth + 1)),
        ),

        // The above *should* cover all cases of irrefutable patterns,
        // but we purposefully don't do any funny business here
        // (such as panicking) because that would obscure rustc's
        // much more informative error message.
        _ => Box::new(iter::empty()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RetTurnTy {
    Result,
    Option,
    Bare,
}

// works with Result<T>, ::std::result::Result<T>, Option<T>, ::std::option::Option<T> and type Result<T> = ::std::result::Result<T, E>;
pub fn return_fallible_ty(output: &ReturnType) -> RetTurnTy {
    let return_ty = match &output {
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => quote! { #ty },
    }
    .to_string();

    if return_ty == "Result" {
        RetTurnTy::Result
    } else if return_ty == "Option" {
        RetTurnTy::Option
    } else {
        RetTurnTy::Bare
    }
}

// Find the type of the value to store.
// Normally it's the same as the return type of the functions, but
// for Options and Results it's the (first) inner type. So for
// Option<u32>, store u32, for Result<i32, String>, store i32, etc.
pub(super) fn find_value_type(
    return_ty: RetTurnTy,
    output: &ReturnType,
    output_ty: TokenStream2,
) -> TokenStream2 {
    match return_ty {
        RetTurnTy::Bare => output_ty,
        _ => match output.clone() {
            ReturnType::Default => {
                panic!("function must return something for result or option attributes")
            }
            ReturnType::Type(_, ty) => {
                if let Type::Path(typepath) = *ty {
                    let segments = typepath.path.segments;
                    if let PathArguments::AngleBracketed(brackets) =
                        &segments.last().unwrap().arguments
                    {
                        let inner_ty = brackets.args.first().unwrap();
                        quote! {#inner_ty}
                    } else {
                        panic!("function return type has no inner type")
                    }
                } else {
                    panic!("function return type too complex")
                }
            }
        },
    }
}

// make the cache key type and block that converts the inputs into the key type
pub(super) fn make_cache_key_type(
    key: &Option<String>,
    convert: &Option<String>,
    cache_type: &Option<String>,
    input_tys: Vec<Type>,
    input_names: &Vec<Ident>,
) -> (TokenStream2, TokenStream2) {
    match (key, convert, cache_type) {
        (Some(key_str), Some(convert_str), _) => {
            let cache_key_ty = parse_str::<Type>(key_str).expect("unable to parse cache key type");

            let key_convert_block =
                parse_str::<Block>(convert_str).expect("unable to parse key convert block");

            (quote! {#cache_key_ty}, quote! {#key_convert_block})
        }
        (None, Some(convert_str), Some(_)) => {
            let key_convert_block =
                parse_str::<Block>(convert_str).expect("unable to parse key convert block");

            (quote! {}, quote! {#key_convert_block})
        }
        (None, None, _) => (quote! {(#(#input_tys),*)}, quote! {(#(#input_names),*)}),
        (Some(_), None, _) => panic!("key requires convert to be set"),
        (None, Some(_), None) => panic!("convert requires key or type to be set"),
    }
}

// if you define arguments as mutable, e.g.
// #[once]
// fn mutable_args(mut a: i32, mut b: i32) -> (i32, i32) {
//     a += 1;
//     b += 1;
//     (a, b)
// }
// then we need to strip off the `mut` keyword from the
// variable identifiers, so we can refer to arguments `a` and `b`
// instead of `mut a` and `mut b`
pub(super) fn get_input_names(inputs: &Punctuated<FnArg, Comma>) -> Vec<(Ident, u8)> {
    inputs
        .iter()
        .flat_map(|input| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => param_names(*pat_type.pat.clone(), 0),
        })
        .collect()
}

// get types for cache key
pub(super) fn get_input_types(
    inputs: &Punctuated<FnArg, Comma>,
    ty_depths_info: &[u8],
) -> Vec<Type> {
    inputs
        .iter()
        .zip(ty_depths_info.iter())
        .map(|(input, depth)| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => ty_from_depth_info(*depth, *pat_type.ty.clone()),
        })
        .collect()
}

pub(super) fn ty_from_depth_info(depth: u8, ty: Type) -> Type {
    if depth == 0 {
        return ty;
    }
    if let Type::Path(p) = ty {
        let mut segments = p.path.segments;
        let last = segments.last_mut().unwrap();
        if let PathArguments::AngleBracketed(brackets) = &mut last.arguments {
            let inner_ty = brackets.args.first_mut().unwrap();
            let inner_ty = match inner_ty {
                GenericArgument::Type(ty) => ty.clone(),
                _ => panic!("GenericArgument not a type"),
            };
            ty_from_depth_info(depth - 1, inner_ty.clone())
        } else {
            panic!("PathArguments not AngleBracketed")
        }
    } else {
        ty
    }
}

//creates call expression for inner function
// eg we had fn foo(a: i32, b: i32) -> i32 { a + b }
// then we create
// ```rs
// foo_inner(a, b).await
// ```
pub(super) fn get_wrapped_type_for_function_call(
    inputs: &Punctuated<FnArg, Comma>,
) -> Vec<TokenStream2> {
    inputs
        .iter()
        .map(|input| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => match *strip_mut_from_pat(pat_type) {
                Pat::Ident(ident) => ident.to_token_stream(),
                Pat::Tuple(tuple) => tuple.to_token_stream(),
                Pat::TupleStruct(tuple_struct) => tuple_struct.to_token_stream(),
                Pat::Struct(struct_pat) => struct_pat.to_token_stream(),
                Pat::Reference(pat_ref) => pat_ref.to_token_stream(),
                _ => panic!("unsupported pattern"),
            },
        })
        .collect()
}
