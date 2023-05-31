use futures::future::join_all;
use shuttle::{
    check_dfs, check_random,
    current::{get_tag_for_current_task, set_tag_for_current_task, Tag},
    future::block_on,
    sync::Mutex,
    thread,
    thread::JoinHandle,
    Config,
};
use std::sync::Arc;
use test_log::test;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Metadata, Subscriber};

fn spawn_some_futures_and_set_tag<F: (Fn(Tag, u64) -> Tag) + Send + Sync>(
    tag_on_entry: Tag,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            shuttle::future::spawn(async move {
                assert_eq!(get_tag_for_current_task(), tag_on_entry);
                let new_tag = f(tag_on_entry, i);
                set_tag_for_current_task(new_tag);
                assert_eq!(get_tag_for_current_task(), new_tag);
            })
        })
        .collect();

    block_on(join_all(threads));

    assert_eq!(get_tag_for_current_task(), tag_on_entry);
}

fn spawn_some_threads_and_set_tag<F: (Fn(Tag, u64) -> Tag) + Send + Sync>(
    tag_on_entry: Tag,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            thread::spawn(move || {
                assert_eq!(get_tag_for_current_task(), tag_on_entry);
                let new_tag = f(tag_on_entry, i);
                set_tag_for_current_task(new_tag);
                assert_eq!(get_tag_for_current_task(), new_tag);
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert_eq!(get_tag_for_current_task(), tag_on_entry);
}

fn spawn_threads_which_spawn_more_threads(
    tag_on_entry: Tag,
    num_threads_first_block: u64,
    num_threads_second_block: u64,
) {
    let mut threads: Vec<_> = (0..num_threads_first_block)
        .map(|i| {
            thread::spawn(move || {
                assert_eq!(get_tag_for_current_task(), tag_on_entry);
                let new_tag = i.into();
                set_tag_for_current_task(new_tag);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|_, _| 123.into(), 13);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|_, x| (x * 13).into(), 7);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) + x).into(), 19);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_futures_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) & x).into(), 17);
                assert_eq!(get_tag_for_current_task(), new_tag);
            })
        })
        .collect();

    assert_eq!(get_tag_for_current_task(), tag_on_entry);

    let new_tag_main_thread = 987654321.into();
    set_tag_for_current_task(new_tag_main_thread);
    assert_eq!(get_tag_for_current_task(), new_tag_main_thread);

    threads.extend(
        (0..num_threads_second_block)
            .map(|i| {
                thread::spawn(move || {
                    assert_eq!(get_tag_for_current_task(), new_tag_main_thread);
                    let new_tag = i.into();
                    set_tag_for_current_task(new_tag);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|_, _| 123.into(), 13);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|_, x| (x * 13).into(), 7);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) + x).into(), 19);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_futures_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) & x).into(), 17);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                })
            })
            .collect::<Vec<_>>(),
    );

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert_eq!(get_tag_for_current_task(), new_tag_main_thread);
}

#[test]
fn threads_which_spawn_threads_which_spawn_threads() {
    check_random(|| spawn_threads_which_spawn_more_threads(Tag::default(), 3, 2), 10)
}

fn spawn_thread_and_set_tag(tag_on_entry: Tag, new_tag: Tag) -> JoinHandle<u64> {
    thread::spawn(move || {
        assert_eq!(get_tag_for_current_task(), tag_on_entry);
        assert_eq!(set_tag_for_current_task(new_tag), tag_on_entry); // NOTE: Assertion with side effect
        assert_eq!(get_tag_for_current_task(), new_tag);
        new_tag.into()
    })
}

fn spawn_and_join() {
    set_tag_for_current_task(42.into());
    let h1 = spawn_thread_and_set_tag(42.into(), 84.into());
    set_tag_for_current_task(50.into());
    let h2 = spawn_thread_and_set_tag(50.into(), 100.into());
    let results = [h1.join().unwrap(), h2.join().unwrap()];
    assert_eq!(results, [84, 100]);
}

#[test]
fn test_spawn_and_join() {
    check_dfs(spawn_and_join, None);
}

fn basic_lock_test() {
    let lock = Arc::new(Mutex::new(0usize));

    let threads = (0..6)
        .map(|i| {
            let lock = lock.clone();
            thread::spawn(move || {
                set_tag_for_current_task((i + 1).into());
                *lock.lock().unwrap() += 1;
            })
        })
        .collect::<Vec<_>>();

    for thread in threads {
        thread.join().unwrap();
    }
}

// Simple `Subscriber` that just checks whether the `runnable` contains `Unset`, `Low`, `Mid` or `Rest`,
// and that they don't contain `TaskId`.
struct RunnableSubscriber {}

impl RunnableSubscriber {
    fn new() -> Self {
        Self {}
    }
}

impl Subscriber for RunnableSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // We don't care about span equality so just use the same identity for everything
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        if metadata.target() == "shuttle::runtime::execution" {
            struct CheckRunnableSubscriber();
            impl Visit for CheckRunnableSubscriber {
                fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
                    assert!(!format!("{value:?}").contains("TaskId"));
                    let f = format!("{value:?}");
                    assert!(f.contains("Unset") || f.contains("Low") || f.contains("Mid") || f.contains("Rest"));
                }
            }
            let mut visitor = CheckRunnableSubscriber();
            event.record(&mut visitor);
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

fn check_with_config<F>(f: F, config: Config, max_iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    use shuttle::{scheduler::RandomScheduler, Runner};

    let scheduler = RandomScheduler::new(max_iterations);

    let runner = Runner::new(scheduler, config);
    runner.run(f);
}

#[test]
fn tracing_tags() {
    let metrics = RunnableSubscriber::new();
    let _guard = tracing::subscriber::set_default(metrics);

    let mut config = Config::default();

    #[derive(Debug)]
    enum TaskType {
        Unset,
        Low,
        Mid,
        Rest(u64),
    }

    config.task_id_and_tag_to_string = Some(|_task_id, tag| {
        let as_enum = match tag.into() {
            x if x == 0 => TaskType::Unset,
            x if x < 3 => TaskType::Low,
            x if x < 5 => TaskType::Mid,
            x => TaskType::Rest(x),
        };
        format!("{as_enum:?}")
    });

    check_with_config(basic_lock_test, config, 10);
}
