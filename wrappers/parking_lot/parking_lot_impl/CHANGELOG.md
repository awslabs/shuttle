# 0.1.3 (Unreleased)

* `Mutex` and `RwLock` unlock as `parking_lot`'s do, on Shuttle's `Fairness::ParkingLot` (#399), instead of handing the lock to the task that waits longest at every unlock (#259). See Shuttle's CHANGELOG. (#376)
* `RwLock` no longer refuses a plain read that `parking_lot` grants while a writer waits behind an upgradable reader (#368), and `try_write` fails while a task waits for the lock. (#376)
* `is_locked`, `is_locked_exclusive` and the `bump` methods read the lock state as `parking_lot`'s do, and a blocking lock of a lock that a panic closed says so when it panics. (#376)
* Requires `shuttle` 0.9.8 and `lock_api` 0.4.6. (#376)

# 0.1.2 (Sep 21, 2026)

* Make `RwLock` upgrades atomic: a writer blocked when `RwLockUpgradableReadGuard::upgrade` starts can no longer be granted the lock part-way through it, so the value an upgradable reader read cannot change across its own upgrade. `try_upgrade` no longer fails spuriously when a writer is merely queued, and `downgrade_to_upgradable` no longer deadlocks against a task waiting to take an upgradable read. The lock is now modelled as permit counts on a single semaphore (shared 1, upgradable a strict majority, exclusive all), so every transition between the three states is atomic. (#351)

# 0.1.1 (Jul 31, 2026)

* Refactor to be built on `lock_api` (#302). Adds `lock_arc`, `read_arc`, `write_arc`, and exposes `RawMutex` and `RawRwLock`.

# 0.1.0

* Initial release