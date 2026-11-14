//! The parity probe in `shuttle-io-uring`'s `tests/parity`, which that crate's
//! tests build against io-uring, built against this crate: each mirrored item
//! has io-uring's signature.

#![cfg(target_os = "linux")]

extern crate shuttle_io_uring_impl as io_uring;

#[path = "../../tests/parity/mod.rs"]
mod parity;

#[test]
fn the_simulation_has_io_urings_signatures() {
    parity::signatures();
}
