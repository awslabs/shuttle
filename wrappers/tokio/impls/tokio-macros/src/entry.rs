//! This file is based on [tokio-macros/src/entry.rs](https://github.com/tokio-rs/tokio/blob/master/tokio-macros/src/entry.rs),
//! but has had the following changes applied. All changes are labeled with `SHUTTLE_CHANGES` markers:
//! 1. Unsupported features (RuntimeFlavor, UnhandledPanic, etc.) have been removed
//! 2. Crate name resolution has been improved to support package renaming and the wrapper scheme
//! 3. The macro output wraps the function body in a `shuttle_tokio::check()` call instead of setting up a tokio runtime

use proc_macro2::{Span, TokenStream, TokenTree};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{quote, quote_spanned, ToTokens};
use syn::parse::{Parse, ParseStream, Parser};
use syn::{braced, Attribute, Ident, ReturnType, Signature, Visibility};

// syn::AttributeArgs does not implement syn::Parse
type AttributeArgs = syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>;

struct Configuration {
    is_test: bool,
}

impl Configuration {
    fn new(is_test: bool) -> Self {
        Configuration { is_test }
    }

    fn macro_name(&self) -> &'static str {
        if self.is_test {
            "shuttle_tokio::test"
        } else {
            "shuttle_tokio::main"
        }
    }
}

fn build_config(input: &ItemFn, args: AttributeArgs, is_test: bool) -> Result<(), syn::Error> {
    if input.sig.asyncness.is_none() {
        let msg = "the `async` keyword is missing from the function declaration";
        return Err(syn::Error::new_spanned(input.sig.fn_token, msg));
    }

    let config = Configuration::new(is_test);
    let macro_name = config.macro_name();

    for arg in args {
        match arg {
            syn::Meta::NameValue(namevalue) => {
                let ident = namevalue
                    .path
                    .get_ident()
                    .ok_or_else(|| syn::Error::new_spanned(&namevalue, "Must have specified ident"))?
                    .to_string()
                    .to_lowercase();
                let _lit = match &namevalue.value {
                    syn::Expr::Lit(syn::ExprLit { lit, .. }) => lit,
                    expr => return Err(syn::Error::new_spanned(expr, "Must be a literal")),
                };
                match ident.as_str() {
                    "worker_threads" => {}
                    "flavor" => {}
                    "start_paused" => {}
                    "core_threads" => {
                        let msg = "Attribute `core_threads` is renamed to `worker_threads`";
                        return Err(syn::Error::new_spanned(namevalue, msg));
                    }
                    "crate" => {}
                    name => {
                        let msg = format!(
                            "Unknown attribute {name} is specified; expected one of: `flavor`, `worker_threads`, `start_paused`, `crate`",
                        );
                        return Err(syn::Error::new_spanned(namevalue, msg));
                    }
                }
            }
            syn::Meta::Path(path) => {
                let name = path
                    .get_ident()
                    .ok_or_else(|| syn::Error::new_spanned(&path, "Must have specified ident"))?
                    .to_string()
                    .to_lowercase();
                let msg = match name.as_str() {
                    "threaded_scheduler" | "multi_thread" => {
                        format!("Set the runtime flavor with #[{macro_name}(flavor = \"multi_thread\")].")
                    }
                    "basic_scheduler" | "current_thread" | "single_threaded" => {
                        format!("Set the runtime flavor with #[{macro_name}(flavor = \"current_thread\")].")
                    }
                    "flavor" | "worker_threads" | "start_paused" => {
                        format!("The `{name}` attribute requires an argument.")
                    }
                    name => {
                        format!(
                            "Unknown attribute {name} is specified; expected one of: `flavor`, `worker_threads`, `start_paused`, `crate`"
                        )
                    }
                };
                return Err(syn::Error::new_spanned(path, msg));
            }
            other => {
                return Err(syn::Error::new_spanned(other, "Unknown attribute inside the macro"));
            }
        }
    }

    Ok(())
}

fn get_env<T>(s: &str, default: T) -> T
where
    T: std::str::FromStr,
{
    std::env::var(s)
        .map(|v| {
            v.parse::<T>()
                .unwrap_or_else(|_| panic!("cannot parse env var {}={} as {}", s, v, std::any::type_name::<T>()))
        })
        .unwrap_or(default)
}

fn get_env_usize(s: &str, default: usize) -> usize {
    get_env(s, default)
}

// SHUTTLE_CHANGES
// Slightly modified from the version in Tokio. Part which sets up the runtime is removed.
// The one in Tokio follows a `let body = quote! {}` scheme, this one wraps the body directly.
fn parse_knobs(mut input: ItemFn, is_test: bool) -> TokenStream {
    input.sig.asyncness = None;

    // If type mismatch occurs, the current rustc points to the last statement.
    let (_last_stmt_start_span, last_stmt_end_span) = {
        let mut last_stmt = input.stmts.last().cloned().unwrap_or_default().into_iter();

        // `Span` on stable Rust has a limitation that only points to the first
        // token, not the whole tokens. We can work around this limitation by
        // using the first/last span of the tokens like
        // `syn::Error::new_spanned` does.
        let start = last_stmt.next().map_or_else(Span::call_site, |t| t.span());
        let end = last_stmt.last().map_or(start, |t| t.span());
        (start, end)
    };

    // SHUTTLE_CHANGES
    // Changed from how Tokio does it. Tokio's approach does not allow package renaming.
    // This checks for an import of one of `shuttle-tokio`, `shuttle-tokio-impl`, or `shuttle-tokio-impl-inner`, and uses the first one found.
    let found_crate = crate_name("shuttle-tokio").unwrap_or_else(|_| {
        crate_name("shuttle-tokio-impl").unwrap_or_else(|_|{
            crate_name("shuttle-tokio-impl-inner")
                .expect("Could not find an import for \"shuttle-tokio\", \"shuttle-tokio-impl\", or \"shuttle-tokio-impl-inner\".")
        })
    });

    let crate_path = match found_crate {
        FoundCrate::Itself => quote!(crate),
        FoundCrate::Name(name) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!( #ident )
        }
    };

    let header = if is_test {
        quote! {
            #[::core::prelude::v1::test]
        }
    } else {
        quote! {}
    };

    // TODO: Enable setting of these, scheduler, etc
    let config = quote! {
        #crate_path::__default_shuttle_config()
    };
    let num_iterations = get_env_usize("SHUTTLE_ITERATIONS", 100);

    let body = input.body();
    let body = match input.sig.output {
        syn::ReturnType::Default => {
            quote_spanned! {last_stmt_end_span=>
                // From Tokio. Not sure if this scheme needs it as well, but doesn't hurt to have it. Can figure out whether it can be removed later.
                #[allow(clippy::expect_used, clippy::diverging_sub_expression)]
                {
                    #crate_path::__check(
                        move || {
                            #crate_path::runtime::Handle::current().block_on(async #body)
                        },
                        #config,
                        #num_iterations,
                    );
                };
            }
        }
        // Tests with a return type becomes an unwrap on the return value, then have their return type wiped.
        syn::ReturnType::Type(_, ref ty) => {
            quote_spanned! {last_stmt_end_span=>
                #[allow(clippy::expect_used, clippy::diverging_sub_expression)]
                {
                    async fn __function_under_test() -> #ty {
                        #body
                    }

                    #crate_path::__check(
                        move || {
                            #crate_path::runtime::Handle::current().block_on(async { __function_under_test().await.unwrap_or_else(|e| panic!("Test failed with error: {e:?}")); })
                        },
                        #config,
                        #num_iterations,
                    );
                };
            }
        }
    };

    // Wipe the return type
    input.sig.output = ReturnType::Default;

    let last_block = quote! {};

    input.into_tokens(header, body, last_block)
}

fn token_stream_with_error(mut tokens: TokenStream, error: syn::Error) -> TokenStream {
    tokens.extend(error.into_compile_error());
    tokens
}

#[cfg(not(test))] // Work around for rust-lang/rust#62127
pub(crate) fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    // If any of the steps for this macro fail, we still want to expand to an item that is as close
    // to the expected output as possible. This helps out IDEs such that completions and other
    // related features keep working.
    let input: ItemFn = match syn::parse2(item.clone()) {
        Ok(it) => it,
        Err(e) => return token_stream_with_error(item, e),
    };

    let config = if input.sig.ident == "main" && !input.sig.inputs.is_empty() {
        let msg = "the main function cannot accept arguments";
        Err(syn::Error::new_spanned(&input.sig.ident, msg))
    } else {
        AttributeArgs::parse_terminated
            .parse2(args)
            .and_then(|args| build_config(&input, args, false))
    };

    match config {
        Ok(()) => parse_knobs(input, false),
        Err(e) => token_stream_with_error(parse_knobs(input, false), e),
    }
}

pub(crate) fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    // If any of the steps for this macro fail, we still want to expand to an item that is as close
    // to the expected output as possible. This helps out IDEs such that completions and other
    // related features keep working.
    let input: ItemFn = match syn::parse2(item.clone()) {
        Ok(it) => it,
        Err(e) => return token_stream_with_error(item, e),
    };
    let config = if let Some(attr) = input.attrs().find(|attr| attr.meta.path().is_ident("test")) {
        let msg = "second test attribute is supplied";
        Err(syn::Error::new_spanned(attr, msg))
    } else {
        AttributeArgs::parse_terminated
            .parse2(args)
            .and_then(|args| build_config(&input, args, true))
    };

    match config {
        Ok(()) => parse_knobs(input, true),
        Err(e) => token_stream_with_error(parse_knobs(input, true), e),
    }
}

struct ItemFn {
    outer_attrs: Vec<Attribute>,
    vis: Visibility,
    sig: Signature,
    brace_token: syn::token::Brace,
    inner_attrs: Vec<Attribute>,
    stmts: Vec<proc_macro2::TokenStream>,
}

impl ItemFn {
    /// Access all attributes of the function item.
    fn attrs(&self) -> impl Iterator<Item = &Attribute> {
        self.outer_attrs.iter().chain(self.inner_attrs.iter())
    }

    /// Get the body of the function item in a manner so that it can be
    /// conveniently used with the `quote!` macro.
    fn body(&self) -> Body<'_> {
        Body {
            brace_token: self.brace_token,
            stmts: &self.stmts,
        }
    }

    /// Convert our local function item into a token stream.
    fn into_tokens(
        self,
        header: proc_macro2::TokenStream,
        body: proc_macro2::TokenStream,
        last_block: proc_macro2::TokenStream,
    ) -> TokenStream {
        let mut tokens = proc_macro2::TokenStream::new();
        header.to_tokens(&mut tokens);

        // Outer attributes are simply streamed as-is.
        for attr in self.outer_attrs {
            attr.to_tokens(&mut tokens);
        }

        // Inner attributes require extra care, since they're not supported on
        // blocks (which is what we're expanded into) we instead lift them
        // outside of the function. This matches the behavior of `syn`.
        for mut attr in self.inner_attrs {
            attr.style = syn::AttrStyle::Outer;
            attr.to_tokens(&mut tokens);
        }

        self.vis.to_tokens(&mut tokens);
        self.sig.to_tokens(&mut tokens);

        self.brace_token.surround(&mut tokens, |tokens| {
            body.to_tokens(tokens);
            last_block.to_tokens(tokens);
        });

        tokens
    }
}

impl Parse for ItemFn {
    #[inline]
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        // This parse implementation has been largely lifted from `syn`, with
        // the exception of:
        // * We don't have access to the plumbing necessary to parse inner
        //   attributes in-place.
        // * We do our own statements parsing to avoid recursively parsing
        //   entire statements and only look for the parts we're interested in.

        let outer_attrs = input.call(Attribute::parse_outer)?;
        let vis: Visibility = input.parse()?;
        let sig: Signature = input.parse()?;

        let content;
        let brace_token = braced!(content in input);
        let inner_attrs = Attribute::parse_inner(&content)?;

        let mut buf = proc_macro2::TokenStream::new();
        let mut stmts = Vec::new();

        while !content.is_empty() {
            if let Some(semi) = content.parse::<Option<syn::Token![;]>>()? {
                semi.to_tokens(&mut buf);
                stmts.push(buf);
                buf = proc_macro2::TokenStream::new();
                continue;
            }

            // Parse a single token tree and extend our current buffer with it.
            // This avoids parsing the entire content of the sub-tree.
            buf.extend([content.parse::<TokenTree>()?]);
        }

        if !buf.is_empty() {
            stmts.push(buf);
        }

        Ok(Self {
            outer_attrs,
            vis,
            sig,
            brace_token,
            inner_attrs,
            stmts,
        })
    }
}

struct Body<'a> {
    brace_token: syn::token::Brace,
    // Statements, with terminating `;`.
    stmts: &'a [TokenStream],
}

impl ToTokens for Body<'_> {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        self.brace_token.surround(tokens, |tokens| {
            for stmt in self.stmts {
                stmt.to_tokens(tokens);
            }
        });
    }
}
