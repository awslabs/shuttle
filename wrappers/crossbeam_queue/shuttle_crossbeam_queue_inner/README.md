# Shuttle support for `crossbeam-queue`

This crate contains the implementation that enables testing of applications that use
`crossbeam-queue` with Shuttle. It should not be depended on directly, depend on
`shuttle-crossbeam-queue` instead.

`ArrayQueue` and `SegQueue` have crossbeam-queue 0.3.14's API. Each operation happens in one step at a
scheduling point, so the queues are linearizable and Shuttle explores every order in which concurrent
operations take effect. A queue's contents belong to the running execution, so a `static` queue starts
every execution empty. See the crate documentation for the details.
