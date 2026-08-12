# shuttle-schedulers

This crate contains schedulers for the [Shuttle](https://crates.io/crates/shuttle)
concurrency testing tool, along with the `check`/`replay` entry points that run a test with them.

## Contents

Each scheduler implements the `Scheduler` trait from
[shuttle-engine](https://crates.io/crates/shuttle-engine):

* `RandomScheduler` — chooses a runnable task uniformly at random at each scheduling point. The
  default choice for most tests.
* `PctScheduler` — the probabilistic concurrency testing algorithm from
  [A Randomized Scheduler with Probabilistic Guarantees of Finding Bugs](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/asplos277-pct.pdf),
  which biases towards the low-preemption schedules that tend to expose real bugs.
* `DfsScheduler` — exhaustively enumerates schedules, in the style of
  [Loom](https://github.com/tokio-rs/loom). Only tractable for small tests.
* `ReplayScheduler` — replays a previously recorded schedule, which is how a failing Shuttle test is
  reproduced and debugged.
* `RoundRobinScheduler` — cycles through runnable tasks in a fixed order. This is more or less never the scheduler you want to use.
* `UrwRandomScheduler` — uniform random walk over the schedule space.
* `UncontrolledNondeterminismCheckScheduler` — detects tests whose behavior depends on
  nondeterminism Shuttle does not control, which would otherwise make failures unreproducible.
* `AnnotationScheduler` — records an annotated schedule for the Shuttle Explorer extension. Requires
  the `annotation` feature.
