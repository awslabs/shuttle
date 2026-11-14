# shuttle-once_cell-inner

Shuttle's implementation of [once_cell](https://crates.io/crates/once_cell)'s `sync` and `race` modules. Do not depend on this crate directly: use [shuttle-once_cell](https://crates.io/crates/shuttle-once_cell) and its `shuttle` feature, which turns this crate on.

## What it models

`sync::OnceCell` and `sync::Lazy` are built on Shuttle's model of `std::sync::OnceLock`, and the `race` types on the same model with once_cell's racy initialization, so that:

- A `static` cell or `Lazy` is empty again at the start of every execution, and its initializer runs again. A cell created with a value (`OnceCell::with_value`, `OnceBox::with_value`, `From`, `Clone`) holds it in every execution.
- Initializing an empty cell takes two scheduling points. Reading one that has a value takes none, and neither does anything done through a `&mut` borrow. A read takes causality from the task that stored the value.
- In `sync`, one initializer runs and the others wait for it. In `race`, every task that finds the cell empty runs its initializer, and the first value stored wins.
- A reentrant `sync` initialization, and a `OnceCell::wait` that no task will satisfy, fail the test with Shuttle's deadlock error, where once_cell blocks forever.

`unsync` is once_cell's own: its cells are `!Sync`, so tasks can't share them, and they can't be in a `static`.

## Limitations

- The methods that reach a stored value need `T: 'static`, as the execution's storage does, and have to run inside a Shuttle execution, `Debug` and `PartialEq` included.
- A value a task stores is dropped when the execution ends, not when the cell is.
- `sync::Lazy`'s initializer runs once per execution, so it is an `F: Fn() -> T` rather than an `FnOnce`, and a `Lazy` is `Sync` if `F` is `Sync`, rather than `Send`.
- `get_unchecked` panics on an empty cell.
- `parking_lot`, `critical-section`, `portable-atomic` and `atomic-polyfill` reach once_cell only; the models replace its implementation.
