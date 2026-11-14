//! Tests of the wrapper as a dependent crate sees it. The workspace builds it
//! without the `shuttle` feature, so these check that it then is io-uring,
//! driving the kernel's ring; `cargo test -p shuttle-io-uring --features
//! shuttle` runs the other half. `shuttle-io-uring-impl`'s tests run the same
//! consumer code, and build the same parity probe, against the simulation.

#![cfg(target_os = "linux")]

/// The crate under test, by the name a dependent crate gives it.
mod io_uring {
    pub use shuttle_io_uring::*;
}

mod consumer;
mod parity;

/// Whether this process can build a kernel ring: a seccomp filter or
/// `kernel.io_uring_disabled` can forbid it, as some containers do.
#[cfg(not(feature = "shuttle"))]
fn kernel_ring_available() -> bool {
    match io_uring::IoUring::new(8) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping: this process cannot build an io_uring: {e}");
            false
        }
    }
}

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
    use super::kernel_ring_available;
    use std::any::TypeId;

    #[test]
    fn types_are_io_urings_own() {
        assert_eq!(
            TypeId::of::<crate::io_uring::IoUring>(),
            TypeId::of::<::io_uring::IoUring>()
        );
        assert_eq!(
            TypeId::of::<crate::io_uring::squeue::Entry>(),
            TypeId::of::<::io_uring::squeue::Entry>()
        );
        assert_eq!(
            TypeId::of::<crate::io_uring::opcode::Writev>(),
            TypeId::of::<::io_uring::opcode::Writev>()
        );
    }

    #[test]
    fn the_parity_probe_builds_against_io_uring() {
        crate::parity::signatures();
    }

    #[test]
    fn consumer_code_runs_on_the_kernel() {
        if kernel_ring_available() {
            crate::consumer::exercise();
        }
    }

    /// The kernel completes ops on its own schedule, so under Shuttle a test
    /// whose control flow depends on when a completion shows up does not
    /// replay, which `check_uncontrolled_nondeterminism` reports. With the
    /// `shuttle` feature the same code replays (`shuttle-io-uring-impl`'s
    /// `consumer_code_replays`).
    #[test]
    fn kernel_completions_are_nondeterminism_under_shuttle() {
        if !kernel_ring_available() {
            return;
        }
        let result = std::panic::catch_unwind(|| {
            shuttle::check_uncontrolled_nondeterminism(
                || {
                    let ring = crate::io_uring::IoUring::new(8).unwrap();
                    let ts = crate::io_uring::types::Timespec::new().nsec(200_000);
                    let timeout = crate::io_uring::opcode::Timeout::new(&ts).build().user_data(1);
                    // SAFETY: the timespec outlives the op, which completes
                    // before the ring drops.
                    unsafe { ring.submission_shared().push(&timeout) }.unwrap();
                    ring.submit().unwrap();
                    // How many times this polls depends on when the kernel's
                    // timer fires, and each poll is a Shuttle scheduling point.
                    // SAFETY: the only CQ handle.
                    while unsafe { ring.completion_shared() }.is_empty() {
                        shuttle::thread::yield_now();
                    }
                },
                20,
            )
        });
        let message = result.expect_err("the kernel's timing is uncontrolled nondeterminism");
        let message = message
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| message.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(message.contains("nondeterminism"), "{message}");
    }
}

#[cfg(feature = "shuttle")]
mod with_shuttle {
    use std::any::TypeId;

    #[test]
    fn types_are_the_simulations() {
        assert_eq!(
            TypeId::of::<crate::io_uring::IoUring>(),
            TypeId::of::<shuttle_io_uring_impl::IoUring>()
        );
        assert_ne!(
            TypeId::of::<crate::io_uring::IoUring>(),
            TypeId::of::<::io_uring::IoUring>()
        );
    }

    #[test]
    fn the_parity_probe_builds_against_the_simulation() {
        crate::parity::signatures();
    }

    #[test]
    fn consumer_code_runs_under_shuttle() {
        shuttle::check_random(crate::consumer::exercise, 50);
    }

    #[test]
    fn consumer_code_replays() {
        shuttle::check_uncontrolled_nondeterminism(crate::consumer::exercise, 20);
    }
}
