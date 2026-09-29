//! Shared code for the `RwLock` parity tests: a reference model of `parking_lot`, an actor that runs
//! the same programs on a real lock, and a harness that compares Shuttle with the model. Each test
//! crate uses only part of this module.

pub mod actor;
pub mod harness;
pub mod reference;
