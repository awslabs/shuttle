# Shuttle support for `crossbeam-deque`

This crate contains the implementation that enables testing of applications that use
`crossbeam-deque` with Shuttle. It should not be depended on directly, depend on
`shuttle-crossbeam-deque` instead.

`Worker`, `Stealer` and `Injector` have crossbeam-deque 0.8.8's API, and `Steal` is crossbeam-deque's
own type. Operations happen at scheduling points; a steal returns `Steal::Retry` exactly when another
operation got in its way, as crossbeam's does, so the schedule decides which steals retry. A queue's
tasks belong to the running execution. See the crate documentation for the details.
