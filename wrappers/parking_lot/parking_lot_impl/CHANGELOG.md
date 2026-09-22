# 0.1.2 (Sep 21, 2026)

* Make `RwLock` upgrades atomic: a writer blocked when `RwLockUpgradableReadGuard::upgrade` starts can no longer be granted the lock part-way through it, so the value an upgradable reader read cannot change across its own upgrade. `try_upgrade` no longer fails spuriously when a writer is merely queued, and `downgrade_to_upgradable` no longer deadlocks against a task waiting to take an upgradable read. The lock is now modelled as permit counts on a single semaphore (shared 1, upgradable a strict majority, exclusive all), so every transition between the three states is atomic. (#351)

# 0.1.1 (Jul 31, 2026)

* Refactor to be built on `lock_api` (#302). Adds `lock_arc`, `read_arc`, `write_arc`, and exposes `RawMutex` and `RawRwLock`.

# 0.1.0

* Initial release