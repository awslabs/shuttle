use futures::future::join_all;
use shuttle::{
    check_dfs, check_random,
    current::{get_tag_for_current_task, get_tag_for_task, set_tag_for_current_task, set_tag_for_task},
    future::block_on,
    sync::Mutex,
    thread,
    thread::JoinHandle,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use test_log::test;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Metadata, Subscriber};

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default, Hash, PartialOrd, Ord)]
pub struct Tag(u64);

impl shuttle::current::Tag for Tag {}

impl From<u64> for Tag {
    fn from(tag: u64) -> Self {
        Tag(tag)
    }
}

impl From<Tag> for u64 {
    fn from(tag: Tag) -> u64 {
        tag.0
    }
}

fn spawn_some_futures_and_set_tag<F: (Fn(Tag, u64) -> Tag) + Send + Sync>(
    tag_on_entry: Tag,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            shuttle::future::spawn(async move {
                assert_eq!(curr_tag(), tag_on_entry);
                let new_tag = f(tag_on_entry, i);
                set_tag_for_current_task(Arc::new(new_tag));
                assert_eq!(curr_tag(), new_tag);
            })
        })
        .collect();

    block_on(join_all(threads));

    assert_eq!(curr_tag(), tag_on_entry);
}

fn spawn_some_threads_and_set_tag<F: (Fn(Tag, u64) -> Tag) + Send + Sync>(
    tag_on_entry: Tag,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            thread::spawn(move || {
                assert_eq!(curr_tag(), tag_on_entry);
                let new_tag = f(tag_on_entry, i);
                set_tag_for_current_task(Arc::new(new_tag));
                assert_eq!(curr_tag(), new_tag);
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert_eq!(curr_tag(), tag_on_entry);
}

fn convert_to_tag(tag: Arc<dyn shuttle::current::Tag>) -> Tag {
    let ptr = Arc::into_raw(tag).cast::<Tag>();
    *unsafe { Arc::from_raw(ptr) }
}

fn curr_tag() -> Tag {
    convert_to_tag(get_tag_for_current_task().unwrap())
}

fn spawn_threads_which_spawn_more_threads(num_threads_first_block: u64, num_threads_second_block: u64) {
    let tag_on_entry = Tag::default();
    set_tag_for_current_task(Arc::new(tag_on_entry));
    let mut threads: Vec<_> = (0..num_threads_first_block)
        .map(|i| {
            thread::spawn(move || {
                assert_eq!(curr_tag(), tag_on_entry);
                let new_tag = i.into();
                set_tag_for_current_task(Arc::new(new_tag));
                assert_eq!(curr_tag(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|_, _| 123.into(), 13);
                assert_eq!(curr_tag(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|_, x| (x * 13).into(), 7);
                assert_eq!(curr_tag(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) + x).into(), 19);
                assert_eq!(curr_tag(), new_tag);
                spawn_some_futures_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) & x).into(), 17);
                assert_eq!(curr_tag(), new_tag);
            })
        })
        .collect();

    assert_eq!(curr_tag(), tag_on_entry);

    let new_tag_main_thread: Tag = 987654321.into();
    set_tag_for_current_task(Arc::new(new_tag_main_thread));
    assert_eq!(curr_tag(), new_tag_main_thread);

    threads.extend(
        (0..num_threads_second_block)
            .map(|i| {
                thread::spawn(move || {
                    assert_eq!(curr_tag(), new_tag_main_thread);
                    let new_tag = i.into();
                    set_tag_for_current_task(Arc::new(new_tag));
                    assert_eq!(curr_tag(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|_, _| 123.into(), 13);
                    assert_eq!(curr_tag(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|_, x| (x * 13).into(), 7);
                    assert_eq!(curr_tag(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) + x).into(), 19);
                    assert_eq!(curr_tag(), new_tag);
                    spawn_some_futures_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) & x).into(), 17);
                    assert_eq!(curr_tag(), new_tag);
                })
            })
            .collect::<Vec<_>>(),
    );

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert_eq!(curr_tag(), new_tag_main_thread);
}

#[test]
fn threads_which_spawn_threads_which_spawn_threads() {
    check_random(|| spawn_threads_which_spawn_more_threads(3, 2), 10)
}

fn spawn_thread_and_set_tag(tag_on_entry: Tag, new_tag: Tag) -> JoinHandle<u64> {
    thread::spawn(move || {
        assert_eq!(curr_tag(), tag_on_entry);
        let old_tag = set_tag_for_current_task(Arc::new(new_tag)).unwrap();
        assert_eq!(convert_to_tag(old_tag), tag_on_entry);
        assert_eq!(curr_tag(), new_tag);
        new_tag.into()
    })
}

fn spawn_and_join() {
    set_tag_for_current_task(Arc::new(Tag::from(42)));
    let h1 = spawn_thread_and_set_tag(42.into(), 84.into());
    set_tag_for_current_task(Arc::new(Tag::from(50)));
    let h2 = spawn_thread_and_set_tag(50.into(), 100.into());
    let results = [h1.join().unwrap(), h2.join().unwrap()];
    assert_eq!(results, [84, 100]);
}

#[test]
fn test_spawn_and_join() {
    check_dfs(spawn_and_join, None);
}

#[derive(Debug)]
enum TaskType {
    Unset,
    Low,
    Mid,
    Rest(u64),
}

impl shuttle::current::Tag for TaskType {}

impl TaskType {
    fn new(i: u64) -> TaskType {
        match i {
            x if x == 0 => TaskType::Unset,
            x if x < 3 => TaskType::Low,
            x if x < 5 => TaskType::Mid,
            x => TaskType::Rest(x),
        }
    }
}

fn basic_lock_test() {
    set_tag_for_current_task(Arc::new(TaskType::new(0)));

    let lock = Arc::new(Mutex::new(0usize));

    let threads = (0..6)
        .map(|i| {
            let lock = lock.clone();
            thread::spawn(move || {
                set_tag_for_current_task(Arc::new(TaskType::new(i + 1)));
                *lock.lock().unwrap() += 1;
            })
        })
        .collect::<Vec<_>>();

    for thread in threads {
        thread.join().unwrap();
    }
}

// Simple `Subscriber` that just checks whether the `runnable` contains `Unset`, `Low`, `Mid` or `Rest`,
// and that they don't contain `TaskId`. All tests have a short "setup phase" before the user is able to
// set the tags, during which traces will contain `TaskId`. Once the setup phase is over, no trace will
// contain `TaskId`.
struct RunnableSubscriber {
    done_with_setup: AtomicBool,
}

impl RunnableSubscriber {
    fn new() -> Self {
        Self {
            done_with_setup: AtomicBool::new(false),
        }
    }
}

impl Subscriber for RunnableSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        if span.metadata().name() == "execution" {
            self.done_with_setup.store(false, Ordering::SeqCst);
        }

        // We don't care about span equality so just use the same identity for everything
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        if metadata.target() == "shuttle::runtime::execution" {
            struct CheckRunnableSubscriber {
                contained_task_id: bool,
            }
            impl Visit for CheckRunnableSubscriber {
                fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
                    let contained_task_id = format!("{value:?}").contains("TaskId");
                    self.contained_task_id = contained_task_id;
                    if !contained_task_id {
                        let f = format!("{value:?}");
                        assert!(f.contains("Unset") || f.contains("Low") || f.contains("Mid") || f.contains("Rest"));
                    }
                }
            }

            let mut visitor = CheckRunnableSubscriber {
                contained_task_id: false,
            };
            event.record(&mut visitor);
            if visitor.contained_task_id {
                assert!(!self.done_with_setup.load(Ordering::SeqCst));
            } else {
                self.done_with_setup.store(true, Ordering::SeqCst)
            }
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[test]
fn tracing_tags() {
    let metrics = RunnableSubscriber::new();
    let _guard = tracing::subscriber::set_default(metrics);

    check_random(basic_lock_test, 10);
}

fn tag_modification_other_task_inner() {
    // Start with a known tag for current task
    set_tag_for_current_task(Arc::new(Tag::from(10)));

    let t1 = thread::spawn(move || {
        // Set the tag for the other thread
        set_tag_for_task(0.into(), Arc::new(Tag::from(42)));
    });

    t1.join().unwrap();

    let my_tag = convert_to_tag(get_tag_for_task(0.into()).unwrap());
    let curr_tag = convert_to_tag(get_tag_for_current_task().unwrap());
    // All tags for task 0 should agree, and be the new value
    assert_eq!(my_tag, curr_tag);
    assert_eq!(curr_tag, Tag::from(42));
}

#[test]
fn test_tag_modification_other_task() {
    check_dfs(tag_modification_other_task_inner, None)
}
