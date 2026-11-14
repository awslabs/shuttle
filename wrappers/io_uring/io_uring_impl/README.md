# Shuttle support for `io-uring`

This crate contains the implementation that enables testing of applications that use
[io-uring](https://crates.io/crates/io-uring) with Shuttle. It should not be depended on
directly, depend on `shuttle-io-uring` instead.

## Implementation

`io-uring` is the kernel interface: it maps the kernel's queues and makes the `io_uring_*` system
calls, and the kernel completes ops on its own schedule, which Shuttle can neither see nor control.
This crate mirrors the `io-uring` 0.7 API over a simulated kernel instead. Each ring has a kernel
task. While ops are in flight it is runnable, and each time Shuttle schedules it, it completes one of
them, chosen by Shuttle, by running the op's system call on the real file descriptor. Ops therefore
stay in flight for as long as Shuttle keeps the kernel task off the CPU, complete in any order, and a
failing schedule replays.

The semantics follow the kernel's: buffers are read and written when an op completes, SQ entries
reach the kernel only when the queue is synced, a completion stores the CQ tail before loading the
flags and signalling the eventfd (both Shuttle atomics), ops may complete inline, cancels may lose
the race, a full CQ holds completions back, and an enter reports what it submitted whatever became
of its wait. Timers, and timed waits, beat I/O in flight only rarely (one time in 256 by default),
which models a device that outlasts its timeout.

On top of that the crate checks what the kernel would not:

- `buffers`: a buffer the submitter changes, or shares with another op when either reads it, while
  the kernel owns it panics at the op that did it, rather than surfacing later as the wrong bytes. A
  read's buffer holds poison while it is in flight, so code that consumes it early sees garbage.
- `faults`: `EAGAIN` refusals with partial transfers, `EIO`, short reads, `ENOSPC`, `EINTR`, and how
  often the device hangs, all drawn with `shuttle::rand` so a schedule replays them.
- `version`: a test can run its rings as Linux 5.4, to drive a consumer's fallbacks for what an old
  kernel lacks: newer opcodes, timed enters, `IORING_FEAT_NODROP` (a full CQ drops completions),
  `IORING_FEAT_SUBMIT_STABLE` and `IORING_FEAT_RW_CUR_POS`.

The crate is forked from `turmoil-io-uring` (MIT; see [LICENSE](LICENSE)), which simulates the ring
on turmoil's clock and filesystem. It keeps that crate's API mirroring, and replaces the clock with
Shuttle's scheduler, the simulated filesystem with real file descriptors, and its simplified
semantics with the kernel's.

## Limitations

- Linux only: on other targets the crate is empty.
- Supported opcodes: `Nop`, `Read`, `Readv`, `Write`, `Writev`, `Fsync`, `Fallocate`, `Timeout` and
  `AsyncCancel`. Any other opcode has no builder.
- `IORING_SETUP_SQPOLL` and `IORING_SETUP_IOPOLL` are rejected; linked and drained SQEs, provided
  buffers and `CQE_SKIP_SUCCESS` complete with `-EINVAL`.
- Deferred task work is not modelled: every completion posts when it happens.
- Each op runs as one system call, so an op is atomic with respect to the others, and nothing models
  power loss or torn writes: the file is a real one.
- Only the simulated kernel's signals wake an `AsyncFd`; the application's own writes to an eventfd
  do not.
