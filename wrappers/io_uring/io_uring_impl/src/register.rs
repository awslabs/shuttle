// This crate began as a fork of turmoil-io-uring 0.1.0 (crates/turmoil-io-uring
// in https://github.com/tokio-rs/turmoil, commit
// 684acc1a8eea3a9cf2c6959dc47b69dba981cac1), Copyright (c) 2022 Carl Lerche,
// which is licensed under the MIT license: see LICENSE-MIT in this crate. This
// file is new in the fork, and is licensed under the Apache License, Version
// 2.0, as the rest of Shuttle is.

//! Registration types, mirroring [`io_uring::register`](https://docs.rs/io-uring/0.7/io_uring/register).

use crate::opcode;

/// Which opcodes the kernel supports, mirroring the real crate's `Probe`.
/// Filled by [`Submitter::register_probe`](crate::Submitter::register_probe)
/// with the opcodes the simulation completes.
#[derive(Debug, Default)]
pub struct Probe {
    supported: Vec<u8>,
}

impl Probe {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn fill(&mut self) {
        self.supported = opcode::SUPPORTED.to_vec();
    }

    pub fn is_supported(&self, opcode: u8) -> bool {
        self.supported.contains(&opcode)
    }
}
