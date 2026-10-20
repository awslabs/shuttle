//! Shared code for the `RwLock` parity tests: a reference model of `parking_lot`, an exact model
//! with its park queue, the programs that the exact model is checked on, an actor that runs the
//! same programs on a real lock, and a harness that compares Shuttle with the models. Each test
//! crate uses only part of this module.

pub mod actor;
pub mod exact;
pub mod harness;
pub mod programs;
pub mod reference;
