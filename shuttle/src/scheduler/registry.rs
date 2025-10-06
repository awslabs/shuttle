use crate::scheduler::{
    DfsScheduler, PctScheduler, RandomScheduler, ReplayScheduler, RoundRobinScheduler, Scheduler, UrwRandomScheduler,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Factory function type for creating schedulers from configuration
pub type SchedulerFactory = Arc<dyn Fn(&config::Config) -> Box<dyn Scheduler + Send> + Send + Sync>;

static REGISTRY: OnceLock<Mutex<HashMap<String, SchedulerFactory>>> = OnceLock::new();

fn get_registry() -> &'static Mutex<HashMap<String, SchedulerFactory>> {
    REGISTRY.get_or_init(|| {
        let mut map = HashMap::new();

        // Register built-in schedulers
        map.insert(
            "random".to_string(),
            Arc::new(|config: &config::Config| {
                Box::new(RandomScheduler::from_config(config)) as Box<dyn Scheduler + Send>
            }) as SchedulerFactory,
        );

        map.insert(
            "dfs".to_string(),
            Arc::new(|config: &config::Config| Box::new(DfsScheduler::from_config(config)) as Box<dyn Scheduler + Send>)
                as SchedulerFactory,
        );

        map.insert(
            "roundrobin".to_string(),
            Arc::new(|config: &config::Config| {
                Box::new(RoundRobinScheduler::from_config(config)) as Box<dyn Scheduler + Send>
            }) as SchedulerFactory,
        );

        map.insert(
            "urw".to_string(),
            Arc::new(|config: &config::Config| {
                Box::new(UrwRandomScheduler::from_config(config)) as Box<dyn Scheduler + Send>
            }) as SchedulerFactory,
        );

        map.insert(
            "pct".to_string(),
            Arc::new(|config: &config::Config| Box::new(PctScheduler::from_config(config)) as Box<dyn Scheduler + Send>)
                as SchedulerFactory,
        );

        map.insert(
            "replay".to_string(),
            Arc::new(|config: &config::Config| {
                Box::new(ReplayScheduler::from_config(config)) as Box<dyn Scheduler + Send>
            }) as SchedulerFactory,
        );

        Mutex::new(map)
    })
}

/// Register a custom scheduler factory with the global registry
///
/// # Example
///
/// ```no_run
/// use shuttle::{register_scheduler, scheduler::Scheduler};
/// use config::Config;
///
/// struct MyScheduler;
/// impl Scheduler for MyScheduler {
///     // ... implementation
/// #   fn new_execution(&mut self) -> Option<shuttle::scheduler::Schedule> { None }
/// #   fn next_task(&mut self, _: &[&shuttle::scheduler::Task], _: Option<shuttle::scheduler::TaskId>, _: bool) -> Option<shuttle::scheduler::TaskId> { None }
/// #   fn next_u64(&mut self) -> u64 { 0 }
/// }
///
/// register_scheduler("my_scheduler", |config| {
///     let iterations = config.get_int("scheduler.iterations").unwrap_or(100) as usize;
///     Box::new(MyScheduler)
/// });
/// ```
pub fn register_scheduler<F>(name: &str, factory: F)
where
    F: Fn(&config::Config) -> Box<dyn Scheduler + Send> + Send + Sync + 'static,
{
    let registry = get_registry();
    let mut map = registry.lock().unwrap();
    map.insert(name.to_string(), Arc::new(factory));
}

/// Create a scheduler from the registry
pub(crate) fn create_scheduler(name: &str, config: &config::Config) -> Box<dyn Scheduler + Send> {
    let registry = get_registry();
    let map = registry.lock().unwrap();
    map.get(name)
        .map(|factory| factory(config))
        .expect("Could not create scheduler")
}
