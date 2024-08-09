//! An example of a Rust footgun where the lifetime of a lock acquired as part of a match scrutinee
//! is extended to the entire match statement, which makes it easy to deadlock.
//!
//! Drawn from https://fasterthanli.me/articles/a-rust-match-made-in-hell with slight changes to
//! switch from `parking_lot` to `std` and from `tokio` to our futures executor.

use shuttle::future as tokio;
use shuttle::sync::{Arc, RwLock};
use std::time::Duration;
use test_log::test;

/// We don't have an equivalent of `tokio::time::sleep`, but yielding has basically the same effect
async fn sleep(_duration: Duration) {
    shuttle::future::yield_now().await;
}

#[derive(Default)]
struct State {
    value: u64,
}

impl State {
    fn foo(&self) -> bool {
        self.value > 0
    }

    fn bar(&self) -> u64 {
        self.value
    }

    fn update(&mut self) {
        self.value += 1;
    }
}

// #[tokio::main(worker_threads = 2)]
async fn main() {
    let state: Arc<RwLock<State>> = Default::default();

    tokio::spawn({
        let state = state.clone();
        async move {
            loop {
                println!("updating...");
                state.write().unwrap().update();
                sleep(Duration::from_millis(1)).await;
            }
        }
    });

    for _ in 0..10 {
        match state.read().unwrap().foo() {
            true => {
                println!("it's true!");
                sleep(Duration::from_millis(1)).await;
                println!("bar = {}", state.read().unwrap().bar());
            }
            false => {
                println!("it's false!");
            }
        }
    }
    println!("okay done");
}

#[test]
#[should_panic(expected = "tried to acquire a RwLock it already holds")]
fn async_match_deadlock() {
    shuttle::check_random(|| tokio::block_on(main()), 1000)
}

#[test]
#[should_panic(expected = "tried to acquire a RwLock it already holds")]
fn async_match_deadlock_replay() {
    // Deterministically replay a deadlocking execution so we can, for example, single-step through
    // it in a debugger.
    shuttle::replay(|| tokio::block_on(main()), "91010ab393f492d4dabde32f202200")
}
