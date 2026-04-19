//! This file is lifted from [tokio/src/macros/select.rs](https://github.com/tokio-rs/tokio/blob/911ab21d7012a50e53971ad1292a9f18de22d4c8/tokio/src/macros/select.rs),
//! and has had the following changes applied to it:
//! 1. Documentation (what is gated behind `doc!`) has been removed
//! 2. `poll_budget_available` is changed to always be `Poll::Ready`, as we don't model cooperative scheduling.
//! 3. `thread_rng_n` is changed to be Shuttle-compatible.

#[macro_export]
macro_rules! select {
    // Uses a declarative macro to do **most** of the work. While it is possible
    // to implement fully with a declarative macro, a procedural macro is used
    // to enable improved error messages.
    //
    // The macro is structured as a tt-muncher. All branches are processed and
    // normalized. Once the input is normalized, it is passed to the top-most
    // rule. When entering the macro, `@{ }` is inserted at the front. This is
    // used to collect the normalized input.
    //
    // The macro only recurses once per branch. This allows using `select!`
    // without requiring the user to increase the recursion limit.

    // All input is normalized, now transform.
    (@ {
        // The index of the future to poll first (in bias mode), or the RNG
        // expression to use to pick a future to poll first.
        start=$start:expr;

        // One `_` for each branch in the `select!` macro. Passing this to
        // `count!` converts $skip to an integer.
        ( $($count:tt)* )

        // Normalized select branches. `( $skip )` is a set of `_` characters.
        // There is one `_` for each select branch **before** this one. Given
        // that all input futures are stored in a tuple, $skip is useful for
        // generating a pattern to reference the future for the current branch.
        // $skip is also used as an argument to `count!`, returning the index of
        // the current select branch.
        $( ( $($skip:tt)* ) $bind:pat = $fut:expr, if $c:expr => $handle:expr, )+

        // Fallback expression used when all select branches have been disabled.
        ; $else:expr

    }) => {{
        // Enter a context where stable "function-like" proc macros can be used.
        //
        // This module is defined within a scope and should not leak out of this
        // macro.
        #[doc(hidden)]
        mod __tokio_select_util {
            // Generate an enum with one variant per select branch
            $crate::select_priv_declare_output_enum!( ( $($count)* ) );
        }

        // `tokio::macros::support` is a public, but doc(hidden) module
        // including a re-export of all types needed by this macro.
        use $crate::macros::support::Future;
        use $crate::macros::support::Pin;
        use $crate::macros::support::Poll::{Ready, Pending};

        const BRANCHES: u32 = $crate::count!( $($count)* );

        let mut disabled: __tokio_select_util::Mask = Default::default();

        // First, invoke all the pre-conditions. For any that return true,
        // set the appropriate bit in `disabled`.
        $(
            if !$c {
                let mask: __tokio_select_util::Mask = 1 << $crate::count!( $($skip)* );
                disabled |= mask;
            }
        )*

        // Create a scope to separate polling from handling the output. This
        // adds borrow checker flexibility when using the macro.
        let mut output = {
            // Store each future directly first (that is, without wrapping the future in a call to
            // `IntoFuture::into_future`). This allows the `$fut` expression to make use of
            // temporary lifetime extension.
            //
            // https://doc.rust-lang.org/1.58.1/reference/destructors.html#temporary-lifetime-extension
            let futures_init = ($( $fut, )+);

            // Safety: Nothing must be moved out of `futures`. This is to
            // satisfy the requirement of `Pin::new_unchecked` called below.
            //
            // We can't use the `pin!` macro for this because `futures` is a
            // tuple and the standard library provides no way to pin-project to
            // the fields of a tuple.
            let mut futures = ($( $crate::macros::support::IntoFuture::into_future(
                        $crate::count_field!( futures_init.$($skip)* )
            ),)+);

            // This assignment makes sure that the `poll_fn` closure only has a
            // reference to the futures, instead of taking ownership of them.
            // This mitigates the issue described in
            // <https://internals.rust-lang.org/t/surprising-soundness-trouble-around-pollfn/17484>
            let mut futures = &mut futures;

            $crate::macros::support::poll_fn(|cx| {
                // Return `Pending` when the task budget is depleted since budget-aware futures
                // are going to yield anyway and other futures will not cooperate.

                // SHUTTLE_CHANGES: The line is the same, but `poll_budget_available` always resolves to `Poll::Ready`
                ::std::task::ready!($crate::macros::support::poll_budget_available(cx));

                // Track if any branch returns pending. If no branch completes
                // **or** returns pending, this implies that all branches are
                // disabled.
                let mut is_pending = false;

                // Choose a starting index to begin polling the futures at. In
                // practice, this will either be a pseudo-randomly generated
                // number by default, or the constant 0 if `biased;` is
                // supplied.
                let start = $start;

                for i in 0..BRANCHES {
                    let branch;
                    #[allow(clippy::modulo_one)]
                    {
                        branch = (start + i) % BRANCHES;
                    }
                    match branch {
                        $(
                            #[allow(unreachable_code)]
                            $crate::count!( $($skip)* ) => {
                                // First, if the future has previously been
                                // disabled, do not poll it again. This is done
                                // by checking the associated bit in the
                                // `disabled` bit field.
                                let mask = 1 << branch;

                                if disabled & mask == mask {
                                    // The future has been disabled.
                                    continue;
                                }

                                // Extract the future for this branch from the
                                // tuple
                                let ( $($skip,)* fut, .. ) = &mut *futures;

                                // Safety: future is stored on the stack above
                                // and never moved.
                                let mut fut = unsafe { Pin::new_unchecked(fut) };

                                // Try polling it
                                let out = match Future::poll(fut, cx) {
                                    Ready(out) => out,
                                    Pending => {
                                        // Track that at least one future is
                                        // still pending and continue polling.
                                        is_pending = true;
                                        continue;
                                    }
                                };

                                // Disable the future from future polling.
                                disabled |= mask;

                                // The future returned a value, check if matches
                                // the specified pattern.
                                #[allow(unused_variables)]
                                #[allow(unused_mut)]
                                match &out {
                                    $crate::select_priv_clean_pattern!($bind) => {}
                                    _ => continue,
                                }

                                // The select is complete, return the value
                                return Ready($crate::select_variant!(__tokio_select_util::Out, ($($skip)*))(out));
                            }
                        )*
                        _ => unreachable!("reaching this means there probably is an off by one bug"),
                    }
                }

                if is_pending {
                    Pending
                } else {
                    // All branches have been disabled.
                    Ready(__tokio_select_util::Out::Disabled)
                }
            }).await
        };

        match output {
            $(
                $crate::select_variant!(__tokio_select_util::Out, ($($skip)*) ($bind)) => $handle,
            )*
            __tokio_select_util::Out::Disabled => $else,
            _ => unreachable!("failed to match bind"),
        }
    }};

    // ==== Normalize =====

    // These rules match a single `select!` branch and normalize it for
    // processing by the first rule.

    (@ { start=$start:expr; $($t:tt)* } ) => {
        // No `else` branch
        $crate::select!(@{ start=$start; $($t)*; panic!("all branches are disabled and there is no else branch") })
    };
    (@ { start=$start:expr; $($t:tt)* } else => $else:expr $(,)?) => {
        $crate::select!(@{ start=$start; $($t)*; $else })
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr, if $c:expr => $h:block, $($r:tt)* ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if $c => $h, } $($r)*)
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr => $h:block, $($r:tt)* ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if true => $h, } $($r)*)
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr, if $c:expr => $h:block $($r:tt)* ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if $c => $h, } $($r)*)
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr => $h:block $($r:tt)* ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if true => $h, } $($r)*)
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr, if $c:expr => $h:expr ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if $c => $h, })
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr => $h:expr ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if true => $h, })
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr, if $c:expr => $h:expr, $($r:tt)* ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if $c => $h, } $($r)*)
    };
    (@ { start=$start:expr; ( $($s:tt)* ) $($t:tt)* } $p:pat = $f:expr => $h:expr, $($r:tt)* ) => {
        $crate::select!(@{ start=$start; ($($s)* _) $($t)* ($($s)*) $p = $f, if true => $h, } $($r)*)
    };

    // ===== Entry point =====

    ($(biased;)? else => $else:expr $(,)? ) => {{
        $else
    }};

    (biased; $p:pat = $($t:tt)* ) => {
        $crate::select!(@{ start=0; () } $p = $($t)*)
    };

    ( $p:pat = $($t:tt)* ) => {
        // Randomly generate a starting point. This makes `select!` a bit more
        // fair and avoids always polling the first future.

        // SHUTTLE_CHANGHES: `thread_rng_n` is changed to be Shuttle-compatible
        $crate::select!(@{ start={ $crate::macros::support::thread_rng_n(BRANCHES) }; () } $p = $($t)*)
    };

    () => {
        compile_error!("select! requires at least one branch.")
    };
}

// And here... we manually list out matches for up to 64 branches... I'm not
// happy about it either, but this is how we manage to use a declarative macro!

#[macro_export]
#[doc(hidden)]
macro_rules! count {
    () => {
        0
    };
    (_) => {
        1
    };
    (_ _) => {
        2
    };
    (_ _ _) => {
        3
    };
    (_ _ _ _) => {
        4
    };
    (_ _ _ _ _) => {
        5
    };
    (_ _ _ _ _ _) => {
        6
    };
    (_ _ _ _ _ _ _) => {
        7
    };
    (_ _ _ _ _ _ _ _) => {
        8
    };
    (_ _ _ _ _ _ _ _ _) => {
        9
    };
    (_ _ _ _ _ _ _ _ _ _) => {
        10
    };
    (_ _ _ _ _ _ _ _ _ _ _) => {
        11
    };
    (_ _ _ _ _ _ _ _ _ _ _ _) => {
        12
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _) => {
        13
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        14
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        15
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        16
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        17
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        18
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        19
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        20
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        21
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        22
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        23
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        24
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        25
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        26
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        27
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        28
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        29
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        30
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        31
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        32
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        33
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        34
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        35
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        36
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        37
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        38
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        39
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        40
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        41
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        42
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        43
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        44
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        45
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        46
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        47
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        48
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        49
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        50
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        51
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        52
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        53
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        54
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        55
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        56
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        57
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        58
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        59
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        60
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        61
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        62
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        63
    };
    (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        64
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! count_field {
    ($var:ident. ) => {
        $var.0
    };
    ($var:ident. _) => {
        $var.1
    };
    ($var:ident. _ _) => {
        $var.2
    };
    ($var:ident. _ _ _) => {
        $var.3
    };
    ($var:ident. _ _ _ _) => {
        $var.4
    };
    ($var:ident. _ _ _ _ _) => {
        $var.5
    };
    ($var:ident. _ _ _ _ _ _) => {
        $var.6
    };
    ($var:ident. _ _ _ _ _ _ _) => {
        $var.7
    };
    ($var:ident. _ _ _ _ _ _ _ _) => {
        $var.8
    };
    ($var:ident. _ _ _ _ _ _ _ _ _) => {
        $var.9
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _) => {
        $var.10
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _) => {
        $var.11
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.12
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.13
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.14
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.15
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.16
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.17
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.18
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.19
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.20
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.21
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.22
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.23
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.24
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.25
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.26
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.27
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.28
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.29
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.30
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.31
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.32
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.33
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.34
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.35
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.36
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.37
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.38
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.39
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.40
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.41
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.42
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.43
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.44
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.45
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.46
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.47
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.48
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.49
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.50
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.51
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.52
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.53
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.54
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.55
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.56
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.57
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.58
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.59
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.60
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.61
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.62
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.63
    };
    ($var:ident. _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) => {
        $var.64
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! select_variant {
    ($($p:ident)::*, () $($t:tt)*) => {
        $($p)::*::_0 $($t)*
    };
    ($($p:ident)::*, (_) $($t:tt)*) => {
        $($p)::*::_1 $($t)*
    };
    ($($p:ident)::*, (_ _) $($t:tt)*) => {
        $($p)::*::_2 $($t)*
    };
    ($($p:ident)::*, (_ _ _) $($t:tt)*) => {
        $($p)::*::_3 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _) $($t:tt)*) => {
        $($p)::*::_4 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_5 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_6 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_7 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_8 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_9 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_10 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_11 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_12 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_13 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_14 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_15 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_16 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_17 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_18 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_19 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_20 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_21 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_22 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_23 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_24 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_25 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_26 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_27 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_28 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_29 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_30 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_31 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_32 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_33 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_34 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_35 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_36 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_37 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_38 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_39 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_40 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_41 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_42 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_43 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_44 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_45 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_46 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_47 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_48 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_49 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_50 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_51 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_52 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_53 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_54 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_55 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_56 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_57 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_58 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_59 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_60 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_61 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_62 $($t)*
    };
    ($($p:ident)::*, (_ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _) $($t:tt)*) => {
        $($p)::*::_63 $($t)*
    };
}
