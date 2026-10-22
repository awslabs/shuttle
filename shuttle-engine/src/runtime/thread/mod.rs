pub mod continuation;

pub(crate) use continuation::switch_between_polls;
pub use continuation::{switch, FirstPollSwitch};
