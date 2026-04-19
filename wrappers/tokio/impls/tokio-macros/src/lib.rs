#![allow(clippy::needless_doctest_main)]
#![warn(missing_debug_implementations, missing_docs, rust_2018_idioms, unreachable_pub)]
#![doc(test(
    no_crate_inject,
    attr(deny(warnings, rust_2018_idioms), allow(dead_code, unused_variables))
))]

//! Macros for use when testing with shuttle-tokio
//! This package is not intended to be depended on directly, and should instead be used via `shuttle-tokio`.
//! Put up an issue if you have a use case for using it directly and we'll add a `shuttle-tokio-macros` crate.
// This file is code-wise a verbatim copy of [tokio-macros/src/lib.rs](https://github.com/tokio-rs/tokio/blob/9e94fa7e15cfe6ebbd06e9ebad4642896620d924/tokio-macros/src/lib.rs),
// but the examples in the comments have been removed.

// This `extern` is required for older `rustc` versions but newer `rustc`
// versions warn about the unused `extern crate`.
#[allow(unused_extern_crates)]
extern crate proc_macro;

mod entry;
mod select;

use proc_macro::TokenStream;

/// Marks async function to be executed by the selected runtime. This macro
/// helps set up a `Runtime` without requiring the user to use
/// [Runtime](../../shuttle-tokio/runtime/struct.Runtime.html) or
/// [Builder](../../shuttle-tokio/runtime/struct.Builder.html) directly.
///
/// Note: This macro is designed to be simplistic and targets applications that
/// do not require a complex setup. If the provided functionality is not
/// sufficient, you may be interested in using
/// [Builder](../../shuttle-tokio/runtime/struct.Builder.html), which provides a more
/// powerful interface.
#[proc_macro_attribute]
#[cfg(not(test))] // Work around for rust-lang/rust#62127
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    entry::main(args.into(), item.into()).into()
}

/// Marks async function to be executed by selected runtime. This macro helps set up a `Runtime`
/// without requiring the user to use [Runtime](../../shuttle-tokio/runtime/struct.Runtime.html) or
/// [Builder](../../shuttle-tokio/runtime/struct.Builder.html) directly.
#[proc_macro_attribute]
#[cfg(not(test))] // Work around for rust-lang/rust#62127
pub fn main_rt(args: TokenStream, item: TokenStream) -> TokenStream {
    entry::main(args.into(), item.into()).into()
}

/// Marks async function to be executed by runtime, suitable to test environment.
/// This macro helps set up a `Runtime` without requiring the user to use
/// [Runtime](../../shuttle-tokio/runtime/struct.Runtime.html) or
/// [Builder](../../shuttle-tokio/runtime/struct.Builder.html) directly.
///
/// Note: This macro is designed to be simplistic and targets applications that
/// do not require a complex setup. If the provided functionality is not
/// sufficient, you may be interested in using
/// [Builder](../../shuttle-tokio/runtime/struct.Builder.html), which provides a more
/// powerful interface.
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    entry::test(args.into(), item.into()).into()
}

/// Marks async function to be executed by runtime, suitable to test environment
///
/// ## Usage
///
/// ```no_run
/// #[shuttle_tokio::test]
/// async fn my_test() {
///     assert!(true);
/// }
/// ```
#[proc_macro_attribute]
pub fn test_rt(args: TokenStream, item: TokenStream) -> TokenStream {
    entry::test(args.into(), item.into()).into()
}

/// Always fails with the error message below.
/// ```text
/// The #[shuttle_tokio::main] macro requires rt or rt-multi-thread.
/// ```
#[proc_macro_attribute]
pub fn main_fail(_args: TokenStream, _item: TokenStream) -> TokenStream {
    syn::Error::new(
        proc_macro2::Span::call_site(),
        "The #[shuttle_tokio::main] macro requires rt or rt-multi-thread.",
    )
    .to_compile_error()
    .into()
}

/// Always fails with the error message below.
/// ```text
/// The #[shuttle_tokio::test] macro requires rt or rt-multi-thread.
/// ```
#[proc_macro_attribute]
pub fn test_fail(_args: TokenStream, _item: TokenStream) -> TokenStream {
    syn::Error::new(
        proc_macro2::Span::call_site(),
        "The #[shuttle_tokio::test] macro requires rt or rt-multi-thread.",
    )
    .to_compile_error()
    .into()
}

/// Implementation detail of the `select!` macro. This macro is **not** intended
/// to be used as part of the public API and is permitted to change.
#[proc_macro]
#[doc(hidden)]
pub fn select_priv_declare_output_enum(input: TokenStream) -> TokenStream {
    select::declare_output_enum(input)
}

/// Implementation detail of the `select!` macro. This macro is **not** intended
/// to be used as part of the public API and is permitted to change.
#[proc_macro]
#[doc(hidden)]
pub fn select_priv_clean_pattern(input: TokenStream) -> TokenStream {
    select::clean_pattern_macro(input)
}
