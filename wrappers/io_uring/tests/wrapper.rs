//! Tests of the wrapper as a dependent crate sees it. The workspace builds it
//! without the `shuttle` feature, so these check that it then is io-uring;
//! `cargo test -p shuttle-io-uring --features shuttle` runs the other half.
//! `shuttle-io-uring-impl`'s tests build the same parity probe against the
//! simulation.

#![cfg(target_os = "linux")]

/// The crate under test, by the name a dependent crate gives it.
mod io_uring {
    pub use shuttle_io_uring::*;
}

mod parity;

#[cfg(not(feature = "shuttle"))]
mod without_shuttle {
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
}
