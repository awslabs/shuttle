# ShuttleTokio

This folder contains the implementation and wrapper for ShuttleTokio, which provides [tokio](https://crates.io/crates/tokio) support to Shuttle.

## How to use

To use it, do something akin to the following in your Cargo.toml:

```
[features]
shuttle = [
   "tokio/shuttle",
]

[dependencies]
tokio = { package = "shuttle-tokio", version = "VERSION_NUMBER" }
```

The code will then behave as before when the `shuttle` feature flag is not provided, and will run with Shuttle-compatible primitives when the `shuttle` feature flag is provided.

## Limitations

The development of ShuttleTokio has come as a consequence of teams at Amazon needing support for model checking code using tokio. What has driven development is the functionality needed by these teams, meaning there are parts of tokio which have not been implemented or which have not been modeled faithfully. This is something to keep in mind if you want to use ShuttleTokio, and there is some likelihood that there will be functionality that will need to be added. We are happy to review PRs if there is functionality you'd like to add to ShuttleTokio.

A non-exhaustive look of current limitations:

- `fs`, `net`, `io`: All of these are currently simply forwarded to tokio in order to make code which uses this functionality compile, though it will not work if one actually tries to use it under Shuttle. This will probably remain the case for `fs` and `io`, though there is a case to be made for adding functionality from `net`. We have thus far done network simulation by building atop of `sync::mpsc`, but it would be nice to have TCP/UDP support out of the box.
- tokio `Runtime` modelling. Our modelling of tokio runtimes is not faithful, and this will be a gap for applications which utilize multiple runtimes or which have some particular runtime setup. **When running under ShuttleTokio, a task is modelled as a thread**, and we do no modeling of task to worker thread mapping (aka it's as if every task has its own worker thread).
- `tokio::test` macro parameters: All of these are allowed to make things compile, but do not do anything when run under Shuttle.
- time modelling (aka `start_paused`). ShuttleTokio does not do anything with regards to modelling of time, apart from providing the primitives, and a very simple built in time model. The plan is to merge https://github.com/awslabs/shuttle/pull/217 and then have ShuttleTokio provide better time modelling via the underlying Shuttle functionality. This also means that the "explicit time management" offered in tokio via `time::advance` will most likely never be the preferred solution for time modelling with ShuttleTokio.
  - TODO: ShuttleTokio needs to be updated to have better out of the box support for time modelling once [#217](https://github.com/awslabs/shuttle/pull/217) is merged.
- `sync::broadcast`. In general `sync` is pretty well supported, but there has yet to be anyone needing `broadcast`, so it has remained unimplemented.
- `RuntimeMetrics`: All of these functions return 0. It would be nice to return correct information. Shuttle should be updated to accommodate.
- Cooperative scheduling. There are some stubs that exist in order to make code which uses it compile, but in general cooperative scheduling is poorly supported, and any usage of it pretends that it doesn't exist.
