# Shuttle support for `uuid`

This crate contains the implementation that enables testing of applications that use [uuid](https://crates.io/crates/uuid) with Shuttle. It should not be depended on directly, depend on `shuttle-uuid` instead.

## Implementation

uuid's random and time-based constructors are associated functions of its own types, so this crate has its own types with uuid 1.29's API: `Uuid`, `NonNilUuid`, `Builder`, `Timestamp`, `ContextV1`, `ContextV7`, and `Hyphenated`, `Simple`, `Urn` and `Braced` in `fmt`. They wrap uuid's types and delegate to them, with the same trait implementations, behind the same features: the formatting traits, `FromStr`, `Hash`, `Ord`, serde (including the `uuid::serde` adapters), arbitrary, borsh, bytemuck, slog and zerocopy. `Timestamp`, `ContextV1` and `ContextV7` are ported from uuid instead, because uuid keeps a timestamp's counter private and draws a context's random start itself. The `uuid!` macro builds this crate's `Uuid`. `Bytes`, `Error`, `Variant`, `Version`, `ClockSequence`, `NoContext` and `ThreadLocalContext` are uuid's own.

The constructors that read randomness or the clock use Shuttle instead:

* **Randomness:** `Uuid::new_v4()` fills 16 bytes from Shuttle's random number generator (`shuttle::rand::try_thread_rng()`) and passes them to `Builder::from_random_bytes`, as uuid does with the operating system's random number generator. `Uuid::new_v7()`, a `ContextV7`'s counter and `ContextV1::new_random()` take their random bits from there too. Shuttle records each random value in the schedule.
* **The clock:** `Timestamp::now()`, and with it `Uuid::now_v1()`, `now_v6()` and `now_v7()`, read a fake clock. In every execution it starts at 2026-01-01T00:00:00Z, so the timestamps say nothing about the actual time, only about the order in which an execution read the clock. Each reading asks Shuttle whether the clock advances, and moves it on by zero or one millisecond. That is a recorded random choice, like any other value from Shuttle's random number generator, so an execution replays. It also means a test sees both two UUIDs in the same millisecond, which is what exercises a `ContextV7`'s counter, and two in consecutive milliseconds.
* **Shared state:** the contexts that `now_v1()`, `now_v6()` and `now_v7()` share, which uuid keeps for the life of the process, are created afresh in every execution, as is the fake clock. They live in the running execution's storage, the same storage Shuttle's own `Once` and `lazy_static` use, so Shuttle drops them when the execution ends. Reading them is not a scheduling point.

The `NonNilUuid` versions of these constructors do the same. Like the rest of Shuttle, they all panic outside of a Shuttle test, and the deterministic constructors (`from_bytes`, `parse_str`, `new_v3`, `new_v5`, `new_v8`, `Builder` and so on) work anywhere.

### Under a scheduler that provides no random data

`shuttle::check_dfs`'s scheduler provides no random data, and nor does any `DfsScheduler` created without `allow_random_data`. There the constructors above draw nothing from Shuttle's generator:

* **Randomness** comes from a generator of the execution's own: a counter of the values drawn from it so far, which starts at zero in every execution, mixed into each value by SplitMix64's output function. No two of its values in an execution are equal, and they pass for random, so two UUIDs of an execution are as unlikely to be equal as two random ones. What an execution gets depends only on the order in which it draws, which the schedule decides, so the same schedule gives the same UUIDs in every run of it, its replay included.
* **The clock** moves on by exactly one millisecond per reading, so the time-based UUIDs of an execution never share a millisecond.

Neither adds a step to the schedule, so making UUIDs adds no choice to the search: `check_dfs` explores the same schedules as it would without them.

## Limitations

* **Not uuid's types:** a crate built on uuid takes uuid's types, not these, so code converts at that boundary with `.into()` (or `From`), under an `#[allow(clippy::useless_conversion)]`, since the same call converts a type to itself without the `shuttle` feature. Each type converts to and from uuid's, except that a `Timestamp` only converts to uuid's, as uuid doesn't expose a timestamp's counter. Trait implementations that other crates have for uuid's types (for example sqlx's, diesel's, or borsh's own `uuid` feature) don't apply to these.
* **uuid 1.29's API:** API that a later uuid release adds is missing under Shuttle until this crate adds it.
* **Exhaustive testing:** under `check_dfs` the random bits are fixed by the schedule and the clock always advances, so the search covers no other values: no two time-based UUIDs in the same millisecond, for example. A `DfsScheduler` created with `allow_random_data` takes the bits and the clock's steps from its generator instead, the same in every execution.
* **`ClockSequence` for Shuttle's `Mutex`:** uuid implements `ClockSequence` for `std::sync::Mutex<C>`, but neither crate can implement it for `shuttle::sync::Mutex<C>`. Lock the mutex and pass a reference to the context instead.
* **Statics:** the fake clock's state is one of the execution's statics, which Shuttle drops last, after the tasks. A time-based constructor called from the destructor of a static dropped after it panics.
