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
