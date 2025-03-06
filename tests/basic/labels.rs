use shuttle::{
    check_dfs, check_random,
    current::{ChildLabelFn, TaskName, get_label_for_task, me, set_label_for_task, set_name_for_task},
    future, thread,
};
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use test_log::test;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Metadata, Subscriber};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Ident(usize);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Parent(usize);

// This test does the following: spawns a tree of tasks with integer ids
// Each task has labels `Parent(i)` and `Ident(j)` where i is the parent's id, and j is the task's id.
// Here is the tree of tasks spawned:
//                    (1)
//                /        \
//             (2)          (3)
//            /   \        /   \
//         (4)    (5)    (6)   (7)
//
// Each leaf task increments a global AtomicUsize (initially 0) by 1
// Each task returns its (Parent, Ident) as a usize pair when it completes.
// The root waits for all tasks to complete, and collects the returned usize pairs. It then checks
// - that the final AtomicUsize value is 4
// - that each (parent, child) pair in the tree above is reported exactly once
//
// This test checks the following properties
// - tasks inherit the Labels of their parent
// - changing child Labels doesn't affect parent Labels
// - child Labels are independent of each other
async fn spawn_tasks(counter: Arc<AtomicUsize>) -> HashSet<(usize, usize)> {
    set_label_for_task(me(), Ident(1));
    let handles = (0..2)
        .map(|i| {
            let counter2 = counter.clone();
            future::spawn(async move {
                // Set this task's Ident to be (2b + i) where (b) is the Ident of the parent
                let Ident(parent) = get_label_for_task(me()).unwrap();
                set_label_for_task(me(), Parent(parent));
                set_label_for_task(me(), Ident(2 * parent + i));

                let handles = (0..2)
                    .map(|j| {
                        let counter3 = counter2.clone();
                        future::spawn(async move {
                            let Ident(parent) = get_label_for_task(me()).unwrap();
                            set_label_for_task(me(), Parent(parent));
                            set_label_for_task(me(), Ident(2 * parent + j));
                            // Increment global counter
                            counter3.fetch_add(1usize, Ordering::SeqCst);
                            // Return (Parent, Ident) for this task
                            let Parent(p) = get_label_for_task::<Parent>(me()).unwrap();
                            let Ident(c) = get_label_for_task::<Ident>(me()).unwrap();
                            (p, c)
                        })
                    })
                    .collect::<Vec<_>>();
                // Read labels again after children have been spawned
                let Parent(p) = get_label_for_task::<Parent>(me()).unwrap();
                let Ident(c) = get_label_for_task::<Ident>(me()).unwrap();
                (p, c, handles)
            })
        })
        .collect::<Vec<_>>();

    let mut values = HashSet::new();
    for h in handles.into_iter() {
        let (a, b, handles) = h.await.unwrap();
        for h2 in handles.into_iter() {
            let v2 = h2.await.unwrap();
            assert!(values.insert(v2));
        }
        assert!(values.insert((a, b)));
    }

    // Validate that root Labels didn't change
    let Ident(c) = get_label_for_task(me()).unwrap();
    assert_eq!(c, 1);

    assert_eq!(get_label_for_task::<Parent>(me()), None);

    values
}

#[test]
fn task_inheritance() {
    check_random(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();

            let seen_values = future::block_on(async move { spawn_tasks(counter2).await });

            // Check final counter value
            assert_eq!(counter.load(Ordering::SeqCst), 2usize.pow(2));

            // Check that we saw all labels
            let expected_values = HashSet::from([(1, 2), (1, 3), (2, 4), (2, 5), (3, 6), (3, 7)]);
            assert_eq!(seen_values, expected_values);
        },
        10_000,
    );
}

// Same test as above, except with threads
fn spawn_threads(counter: Arc<AtomicUsize>) -> HashSet<(usize, usize)> {
    set_label_for_task(me(), Ident(1));
    let handles = (0..2)
        .map(|i| {
            let counter2 = counter.clone();
            thread::spawn(move || {
                // Set this task's Ident to be (2b + i) where (b) is the Ident of the parent
                let Ident(parent) = get_label_for_task(me()).unwrap();
                set_label_for_task(me(), Parent(parent));
                set_label_for_task(me(), Ident(2 * parent + i));

                let handles: Vec<thread::JoinHandle<(usize, usize)>> = (0..2)
                    .map(|j| {
                        let counter3 = counter2.clone();
                        thread::spawn(move || {
                            let Ident(parent) = get_label_for_task(me()).unwrap();
                            set_label_for_task(me(), Parent(parent));
                            set_label_for_task(me(), Ident(2 * parent + j));
                            // Increment global counter
                            counter3.fetch_add(1usize, Ordering::SeqCst);
                            // Return (Parent, Ident) for this task
                            let Parent(p) = get_label_for_task::<Parent>(me()).unwrap();
                            let Ident(c) = get_label_for_task::<Ident>(me()).unwrap();
                            (p, c)
                        })
                    })
                    .collect::<Vec<_>>();
                // Read labels again after children have been spawned
                let Parent(p) = get_label_for_task::<Parent>(me()).unwrap();
                let Ident(c) = get_label_for_task::<Ident>(me()).unwrap();
                (p, c, handles)
            })
        })
        .collect::<Vec<_>>();

    let mut values = HashSet::new();
    for h in handles.into_iter() {
        let (a, b, handles) = h.join().unwrap();
        for h2 in handles.into_iter() {
            let (c, d) = h2.join().unwrap();
            assert!(values.insert((c, d)));
        }
        assert!(values.insert((a, b)));
    }

    // Validate that root Labels didn't change
    let Ident(c) = get_label_for_task(me()).unwrap();
    assert_eq!(c, 1);

    assert_eq!(get_label_for_task::<Parent>(me()), None);

    values
}

#[test]
fn thread_inheritance() {
    check_random(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();

            let seen_values = spawn_threads(counter2);

            // Check final counter value
            assert_eq!(counter.load(Ordering::SeqCst), 2usize.pow(2));

            // Check that we saw all labels
            let expected_values = HashSet::from([(1, 2), (1, 3), (2, 4), (2, 5), (3, 6), (3, 7)]);
            assert_eq!(seen_values, expected_values);
        },
        10_000,
    );
}

// Check that a task can modify another task's Label; in this example,
// the spawned task modifies its parent's Label.
#[test]
fn label_modify() {
    check_dfs(
        || {
            // Start with a known label for current task
            set_label_for_task(me(), Ident(0));

            let parent_id = me();

            let child = thread::spawn(move || {
                // Set the label for the other thread
                set_label_for_task(parent_id, Ident(1));
                assert_ne!(me(), parent_id);
                // Return my id
                get_label_for_task::<Ident>(me()).unwrap()
            });

            let child_id = child.join().unwrap();
            let my_label = get_label_for_task::<Ident>(me()).unwrap();

            assert_eq!(my_label, Ident(1)); // parent id has changed
            assert_eq!(child_id, Ident(0)); // child id is the parent's original id
        },
        None,
    );
}

// The following tests exercise the functionality provided by `ChildLabelFn`.
// The main task (which is always named "main-thread(0)" by Shuttle) creates 3 child
// tasks.  We test two scenarios:
// (1) the parent sets up a `ChildLabelFn` to assign the child names at creation time
//     (so that the names are in effect as soon as the child is created)
// (2) the child tasks assign names to themselves by calling `set_task_name` as the
//     first statement in their code,
//
// We use a custom tracing subscriber to monitor the list of `enabled` tasks that
// is logged by Shuttle at each scheduling point.  (This also allows us to check
// that Shuttle uses the user-assigned `TaskName` when generating debug output.)
//
// The two tests below check that:
// - In scenario (1), the only names seen in the `runnable` list are `main-thread(0)`
//   or names starting with `Child(`
// - In scenario (2) the `runnable` list contains other names

async fn label_fn_inner(set_name_before_spawn: bool) {
    // Spawn 3 children
    let handles = (0..3).map(|_| {
        if set_name_before_spawn {
            // Test scenario (2)
            set_label_for_task(
                me(),
                ChildLabelFn(Arc::new(|_task_id, labels| {
                    labels.insert(TaskName::from("Child"));
                })),
            );
        }
        future::spawn(async move {
            if !set_name_before_spawn {
                // Test scenario (1)
                set_name_for_task(me(), TaskName::from("Child"));
            }
            shuttle::future::yield_now().await;
        })
    });

    for h in handles {
        h.await.unwrap();
    }
}

#[test]
fn test_tracing_with_label_fn() {
    let metrics = RunnableSubscriber {};
    let _guard = tracing::subscriber::set_default(metrics);

    check_random(
        || {
            future::block_on(async { label_fn_inner(true).await });
        },
        10,
    );
}

#[test]
#[should_panic(expected = "assertion failed")]
fn test_tracing_without_label_fn() {
    let metrics = RunnableSubscriber {};
    let _guard = tracing::subscriber::set_default(metrics);

    check_random(
        || {
            future::block_on(async { label_fn_inner(false).await });
        },
        1, // even one execution is enough to fail the assertion
    );
}

// Custom Subscriber implementation to monitor and check debug output generated by Shuttle.
struct RunnableSubscriber;

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
        let target = metadata.target();
        if target.contains("shuttle") && target.ends_with("::runtime::execution") {
            let fields: &tracing::field::FieldSet = metadata.fields();
            if fields.iter().any(|f| f.name() == "runnable") {
                struct CheckRunnableSubscriber;
                impl Visit for CheckRunnableSubscriber {
                    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                        if field.name() == "runnable" {
                            // The following code relies on the fact that the list of runnable tasks is a SmallVec which is reported
                            // in debug output in the format "[main-thread(0), first-task-name(3), other-task-name(17)]" etc.
                            let value = format!("{:?}", value).replace('[', "").replace(']', "");
                            let v1 = value.split(',');
                            // The following assertion fails if a `ChildLabelFn` is not used to set child task names.
                            assert!(
                                v1.map(|s| s.trim())
                                    .all(|s| (s == "main-thread(0)") || s.starts_with("Child("))
                            );
                        }
                    }
                }

                let mut visitor = CheckRunnableSubscriber {};
                event.record(&mut visitor);
            }
        }
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}
