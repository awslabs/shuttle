# Shuttle support for `uuid`

This crate contains the wrapper that enables testing of applications that use [uuid](https://crates.io/crates/uuid) with Shuttle.

## How to use

To use it, add the following in your Cargo.toml:

```
[features]
shuttle = [
   "uuid/shuttle",
]

[dependencies]
uuid = { package = "shuttle-uuid", version = "VERSION_NUMBER", features = ["v4"] }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided. Under the flag, the constructors that read randomness or the clock (`Uuid::new_v4()`, `now_v7()` and so on) use Shuttle's random number generator and a fake clock, so the same seed or schedule gives the same UUIDs, and a failing schedule replays with the UUIDs it failed with. Under `shuttle::check_dfs`, whose scheduler provides no random data, they take their random bits from a deterministic generator of each execution's own instead, and the fake clock moves on by exactly one millisecond per reading, so making UUIDs adds no choice to the search.

Under the flag, `Uuid` and the other types are Shuttle's own, not uuid's. Code that hands a UUID to a crate built on uuid converts it with `.into()`, which also compiles without the flag. Write those calls as `#[allow(clippy::useless_conversion)] let real = uuid.into();`, or put the attribute on the enclosing function: without the flag the conversion is from a type to itself, which Clippy's `useless_conversion` lint reports, and with it the conversion is a real one.

## Limitations

For the list of current limitations, see the [shuttle-uuid-inner](https://crates.io/crates/shuttle-uuid-inner) inner crate.
