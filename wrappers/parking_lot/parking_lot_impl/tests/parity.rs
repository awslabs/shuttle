//! The same consumer source as `shuttle-parking_lot`'s `tests/wrapper.rs`, compiled against **real**
//! `parking_lot` 0.12 and run on OS threads. It passing is the API parity check: every signature,
//! every `const` constructor and every trait bound the module names has to exist in both crates with
//! the same shape. The auto-trait table it prints is compared with the Shuttle build's.

pub use parking_lot;
pub use std::thread;

#[path = "../../tests/consumer/mod.rs"]
mod consumer;

#[test]
fn the_consumer_code_runs_on_real_parking_lot() {
    consumer::exercise();
    consumer::signatures();
}

#[test]
fn auto_traits() {
    println!("{}", consumer::auto_traits());
}
