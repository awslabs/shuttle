# Shuttle

[![crates.io](https://img.shields.io/crates/v/shuttle.svg)](https://crates.io/crates/shuttle)
[![docs.rs](https://docs.rs/shuttle/badge.svg)](https://docs.rs/shuttle)
[![Tests](https://github.com/awslabs/shuttle/actions/workflows/tests.yml/badge.svg)](https://github.com/awslabs/shuttle/actions/workflows/tests.yml)

Shuttle is a library for testing concurrent Rust code. It is an implementation of a number of
*randomized concurrency testing* techniques, including
[A Randomized Scheduler with Probabilistic Guarantees of Finding Bugs](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/asplos277-pct.pdf).

## Getting started

Consider this simple piece of concurrent code:

```rust
use std::sync::{Arc, Mutex};
use std::thread;

let lock = Arc::new(Mutex::new(0u64));
let lock2 = lock.clone();

thread::spawn(move || {
    *lock.lock().unwrap() = 1;
});

assert_eq!(0, *lock2.lock().unwrap());
```

There is an obvious race condition here: if the spawned thread runs before the assertion, the
assertion will fail. But writing a unit test that finds this execution is tricky. We could run
the test many times and try to "get lucky" by finding a failing execution, but that's not a very
reliable testing approach. Even if the test does fail, it will be difficult to debug: we won't
be able to easily catch the failure in a debugger, and every time we make a change, we will need
to run the test many times to decide whether we fixed the issue.

### Randomly testing concurrent code with Shuttle

Shuttle avoids this issue by controlling the scheduling of each thread in the program, and
scheduling those threads *randomly*. By controlling the scheduling, Shuttle allows us to
reproduce failing tests deterministically. By using random scheduling, with appropriate
heuristics, Shuttle can still catch most (non-adversarial) concurrency bugs even though it is
not an exhaustive checker.

A Shuttle version of the above test just wraps the test body in a call to Shuttle's
`check_random` function, and replaces the concurrency-related imports from `std` with imports
from `shuttle`:

```rust
use shuttle::sync::{Arc, Mutex};
use shuttle::thread;

shuttle::check_random(|| {
    let lock = Arc::new(Mutex::new(0u64));
    let lock2 = lock.clone();

    thread::spawn(move || {
        *lock.lock().unwrap() = 1;
    });

    assert_eq!(0, *lock2.lock().unwrap());
}, 100);
```

This test detects the assertion failure with extremely high probability (over 99.9999%).

Shuttle is inspired by the [Loom](https://github.com/tokio-rs/loom) library for
testing concurrent Rust code.  Shuttle focuses on randomized testing, rather
than the exhaustive testing that Loom offers. This is a soundness—scalability
trade-off: Shuttle is not sound (a passing Shuttle test does not prove the code
is correct), but it scales to much larger test cases than Loom. Empirically,
randomized testing is successful at finding most concurrency bugs, which tend
not to be adversarial.
## License

This project is licensed under the Apache-2.0 License.

## Security

See [CONTRIBUTING](CONTRIBUTING.md#security-issue-notifications) for more information.
