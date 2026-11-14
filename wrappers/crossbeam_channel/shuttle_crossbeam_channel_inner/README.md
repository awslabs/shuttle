# Shuttle support for `crossbeam-channel`

This crate contains the implementation that enables testing of applications that use [crossbeam-channel](https://crates.io/crates/crossbeam-channel) with Shuttle. It should not be depended on directly, depend on `shuttle-crossbeam-channel` instead.

## Implementation

The crate has crossbeam-channel 0.5.17's API: `unbounded`, `bounded` (including zero capacity), `after`, `at`, `tick`, `never`, `Sender`, `Receiver`, their iterators, `Select`, `SelectedOperation`, and the `select!` and `select_biased!` macros, which are crossbeam's own, lifted from crossbeam-channel 0.5.17. The error types are crossbeam-channel's.

* **Scheduling points:** every send, receive and selection takes one at its start, and so do `len`, `is_empty` and `is_full`, and dropping the last handle of a side, which disconnects the channel. A blocked operation is a blocked Shuttle task.
* **Which task gets a message:** an unbounded or bounded channel wakes every task blocked on the side that can now go on, and the scheduler decides which of them gets the message. A zero-capacity channel hands a message to the operation on the other side that has waited longest, as crossbeam's does.
* **Selection:** among the ready operations, a selection takes one drawn from Shuttle's random number generator, which records the draw in the schedule. It draws only when more than one operation is ready; a biased selection never draws.
* **Time:** a timed operation (`recv_timeout`, `send_deadline`, `select!` with `default(timeout)`, `ready_timeout` and the others) times out only when no task can run, and the waits with the earliest deadline time out first. The timeout moves `shuttle-time`'s clock to its deadline. The timers of `after`, `at` and `tick` fire the same way. This is when the timers of Shuttle's time models fire, and when tokio's paused clock moves.
* **State:** a channel's state belongs to the execution that created it, so a channel that outlives it starts over in the next execution that uses it.

## Limitations

* A timeout never fires while another task could still have made progress, so Shuttle does not explore a reply that arrives just after its request timed out. A task that spins while it waits for a timeout keeps a task runnable, so the timeout never fires.
* Under Shuttle, a channel's `Instant` is `shuttle-time`'s, so a deadline computed from `std::time::Instant` does not compile.
* `shuttle::check_dfs` refuses random data, so a selection with more than one ready operation panics there, as Shuttle's other randomness does.
* Every operation must run inside a Shuttle test, and so must the constructors, except `never()`.
