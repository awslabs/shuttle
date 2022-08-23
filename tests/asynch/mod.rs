// It's convenient to not specify eval order for `await`s in our tests
#![allow(clippy::mixed_read_write_in_expression)]

mod basic;
mod channel;
mod countdown_timer;
mod pct;
mod waker;
