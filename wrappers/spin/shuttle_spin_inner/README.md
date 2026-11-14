# Shuttle support for `spin`

This crate contains the implementation that enables testing of applications that use
[spin](https://crates.io/crates/spin) 0.10 with Shuttle. It should not be depended on directly,
depend on `shuttle-spin` instead.

## Implementation

Each of spin's primitives is an atomic word and a loop that retries an operation on the word. These
implementations keep the same word, with spin's encoding, and run spin's operations on it, so they
have spin's API and its rules: an upgradeable guard keeps new readers out, a ticket lock serves its
waiters in the order they drew their tickets, a `Once` whose initializer panicked stays poisoned.
What differs is how a task waits, and where the state lives:

* **A waiting task blocks instead of spinning.** Shuttle runs every task of a test on one thread,
  so a task that spins for a lock another task holds never lets that task run. Here a task whose
  operation fails blocks until another task changes the word, and then retries. That reaches the
  same states as spinning: until the word changes, a retry fails again. If every task is blocked,
  Shuttle reports a deadlock.
* **Each operation on the word is a scheduling point**, as each operation of one of Shuttle's
  atomics is: taking a lock, trying to, releasing a guard, an upgrade or downgrade, the `force_*`
  methods, and the reads `is_locked`, `reader_count` and `writer_count`. Reading an initialized
  `Once` takes none, as Shuttle's `OnceLock` does, and neither do `get_mut`, `into_inner` and
  `Debug`.
* **State resets per execution.** The word lives in the storage of the running execution, so a
  `static` lock is unlocked at the start of every execution, with no waiters, and a `static` `Once`
  or `Lazy` is uninitialized again, its initializer running once in each execution.

## Limitations

* **A lock's value resets per execution only when it needs no dropping.** A sized value with no
  drop glue (a number, a `bool`, `Option<&'static str>`) is copied into each execution, so a
  `static` lock starts every execution with the value it was created with. A value that owns
  something (a `Vec`, a `String`, a `Box`) is used where it is and carries over into the next
  execution, as it would in a plain `static`.
* `Once` and `Lazy` keep their values in the execution's storage, so `call_once` and
  `try_call_once` require `T: 'static`, and `Lazy`'s initializer must be `Fn` rather than `FnOnce`,
  because it runs in every execution. `Lazy` is `Sync` only if its initializer is.
* `try_lock_weak`, `try_write_weak` and `try_upgrade_weak` never fail spuriously.
* A `FairMutex`'s `lock` never becomes a starving user on its own. spin's does after spinning 1,024
  times; Shuttle has no measure of how long a task has waited. `FairMutex::starve` works as in
  spin.
* The relax strategies are never called by a lock, since no lock spins. Calling one yourself takes
  a scheduling point, where spin's `Spin` and `Loop` take none, so that a hand-written wait loop
  can end.
* **Two executions can't hold one lock at once**, even both to read it, and even if its value is
  copied into each, as a lock can guard data outside of itself (the `Mutex<()>` under a `lock_api`
  lock). An execution that takes a lock while a guard of another execution holds it panics, as
  Shuttle's other locks do, saying whose guard it is: one of a test that runs at the same time on
  another thread and uses the same `static` (such tests have to take turns), or one that an ended
  execution forgot with `mem::forget`, which holds the lock for good. A guard leaked with its
  `leak` method keeps the lock locked in later executions instead, as spin does.
* `no_std` is not supported: Shuttle needs `std`. The `portable-atomic` features are accepted and
  have no effect, since Shuttle's model replaces the atomics.
