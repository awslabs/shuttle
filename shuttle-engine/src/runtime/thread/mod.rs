pub mod continuation;

pub(crate) use continuation::switch_between_polls;
pub use continuation::{switch, switch_before_poll, switch_if_not_stopping, switch_if_not_stopping_with, switch_with};
