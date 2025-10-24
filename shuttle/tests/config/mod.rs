use shuttle::scheduler::{DfsScheduler, PctScheduler, RandomScheduler, UrwRandomScheduler};
use shuttle::{load_global_config, load_global_config_from};
use std::env;
use std::sync::{Arc, RwLock};

static ENV_LOCK: RwLock<()> = RwLock::new(());

#[test]
fn test_config_from_env() {
    let settings = {
        let _guard = ENV_LOCK.write().unwrap();
        env::set_var("SHUTTLE.STACK_SIZE", "0");
        let settings = load_global_config();
        env::remove_var("SHUTTLE.STACK_SIZE");
        settings
    };

    let config = shuttle::Config::from_global_config(&settings);

    assert_eq!(config.stack_size, 0);
}

#[test]
fn test_config_from_file() {
    let settings = {
        let _guard = ENV_LOCK.read().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/config/shuttle_dfs.toml");
        load_global_config_from(Some(path))
    };
    assert_eq!(settings.get_string("scheduler.type").unwrap(), "dfs");
}

#[test]
fn test_custom_scheduler_custom_config_field() {
    use shuttle::register_scheduler;
    use shuttle::scheduler::{Schedule, Scheduler, Task, TaskId};

    struct FooScheduler {
        _foo: String,
    }

    impl Scheduler for FooScheduler {
        fn new_execution(&mut self) -> Option<Schedule> {
            Some(Schedule::new(0))
        }

        fn next_task(&mut self, runnable: &[&Task], _: Option<TaskId>, _: bool) -> Option<TaskId> {
            runnable.first().map(|t| t.id())
        }

        fn next_u64(&mut self) -> u64 {
            0
        }
    }

    register_scheduler("foo_scheduler", |config| {
        let foo = config.get_string("scheduler.foo").unwrap();
        Box::new(FooScheduler { _foo: foo })
    });
    let settings = {
        let _guard = ENV_LOCK.write().unwrap();
        env::set_var("SHUTTLE.SCHEDULER.TYPE", "foo_scheduler");
        env::set_var("SHUTTLE.SCHEDULER.FOO", "bar");

        let settings = load_global_config();
        env::remove_var("SHUTTLE.SCHEDULER.TYPE");
        env::remove_var("SHUTTLE.SCHEDULER.FOO");
        settings
    };
    assert_eq!(settings.get_string("scheduler.type").unwrap(), "foo_scheduler");
    assert_eq!(settings.get_string("scheduler.foo").unwrap(), "bar");
}

mod main_task_only_scheduler {
    use shuttle::scheduler::{Schedule, Scheduler, Task, TaskId};
    use std::sync::Once;

    static REGISTER: Once = Once::new();

    pub struct MainTaskOnlyScheduler {
        iterations: usize,
        current: usize,
    }

    impl Scheduler for MainTaskOnlyScheduler {
        fn new_execution(&mut self) -> Option<Schedule> {
            if self.current < self.iterations {
                self.current += 1;
                Some(Schedule::new(0))
            } else {
                None
            }
        }

        fn next_task(&mut self, runnable: &[&Task], current: Option<TaskId>, _: bool) -> Option<TaskId> {
            if current.is_none() {
                Some(runnable[0].id())
            } else {
                None
            }
        }

        fn next_u64(&mut self) -> u64 {
            0
        }
    }

    pub fn register() {
        REGISTER.call_once(|| {
            shuttle::register_scheduler("main_task_only", |config| {
                let iterations = config.get_int("scheduler.iterations").unwrap_or(1) as usize;
                Box::new(MainTaskOnlyScheduler { iterations, current: 0 })
            });
        });
    }
}

#[test]
fn test_run_custom_scheduler() {
    main_task_only_scheduler::register();

    let thread1_ran_outer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread1_ran_inner = thread1_ran_outer.clone();
    {
        let _guard = ENV_LOCK.write().unwrap();
        env::set_var("SHUTTLE.SCHEDULER.TYPE", "main_task_only");
        env::set_var("SHUTTLE.SCHEDULER.ITERATIONS", "1");

        shuttle::check(move || {
            use shuttle::sync::atomic::{AtomicBool, Ordering};
            use shuttle::sync::Arc;

            let thread1_ran = Arc::new(AtomicBool::new(false));
            let t1 = thread1_ran.clone();

            shuttle::thread::spawn(move || {
                t1.store(true, Ordering::SeqCst);
            });

            shuttle::thread::yield_now();

            _ = thread1_ran_inner.fetch_or(thread1_ran.load(Ordering::SeqCst), Ordering::SeqCst);
        });

        env::remove_var("SHUTTLE.SCHEDULER.TYPE");
        env::remove_var("SHUTTLE.SCHEDULER.ITERATIONS");
    }
    assert!(!thread1_ran_outer.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn test_config_example_file() {
    let settings = {
        let _guard = ENV_LOCK.read().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shuttle_config_example.toml");
        load_global_config_from(Some(path))
    };

    let expected = crate::Config::default();
    let actual = crate::Config::from_global_config(&settings);

    assert_eq!(expected, actual);

    RandomScheduler::from_config(&settings);
}

#[test]
fn test_config_example_scheduler_dfs() {
    let settings = {
        let _guard = ENV_LOCK.write().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shuttle_config_example.toml");
        env::set_var("SHUTTLE.SCHEDULER.TYPE", "dfs");
        let settings = load_global_config_from(Some(path));
        env::remove_var("SHUTTLE.SCHEDULER.TYPE");
        settings
    };
    DfsScheduler::from_config(&settings);
}

#[test]
fn test_config_example_scheduler_urw() {
    let settings = {
        let _guard = ENV_LOCK.write().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shuttle_config_example.toml");
        env::set_var("SHUTTLE.SCHEDULER.TYPE", "urw");
        let settings = load_global_config_from(Some(path));
        env::remove_var("SHUTTLE.SCHEDULER.TYPE");
        settings
    };
    UrwRandomScheduler::from_config(&settings);
}

#[test]
fn test_config_example_scheduler_pct() {
    let settings = {
        let _guard = ENV_LOCK.write().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shuttle_config_example.toml");
        env::set_var("SHUTTLE.SCHEDULER.TYPE", "pct");
        let settings = load_global_config_from(Some(path));
        env::remove_var("SHUTTLE.SCHEDULER.TYPE");
        settings
    };
    PctScheduler::from_config(&settings);
}
