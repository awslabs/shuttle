//! Annotated schedules. When an execution is scheduled using the
//! [`crate::scheduler::AnnotationScheduler`], Shuttle will produce a file that contains
//! additional information about the execution, such as the kind of step that
//! was taken (was a task created, were permits acquired from a semaphore, etc)
//! as well as the task's vector clocks and thus any causal dependence between
//! the tasks. The resulting file can be visualized using the Shuttle Explorer
//! IDE extension.

// TODO: the types defined here with `derive(Serialize)` are all parsed from
//       JSON output by Shuttle Explorer; if any changes are made, they should
//       also be reflected in the parsing
// TODO: introduce version numbers to make sure breaking changes are noticed

cfg_if::cfg_if! {
    if #[cfg(feature = "annotation")] {
        use crate::runtime::{
            execution::ExecutionState,
            task::{clock::VectorClock, Task, TaskId},
        };
        use serde::Serialize;
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::thread_local;

        thread_local! {
            static ANNOTATION_STATE: RefCell<Option<AnnotationState>> = const { RefCell::new(None) };
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
        pub(crate) struct ObjectId(usize);

        pub(crate) const DUMMY_OBJECT_ID: ObjectId = ObjectId(usize::MAX);

        pub(crate) const ANNOTATION_VERSION: usize = 0;

        /// Information about a file path found in one or more backtraces in the
        /// annotated schedule. The path is stored in this type; instances of this
        /// type are stored in the `files` vector in `AnnotationState`, and backtrace
        /// frames then refer to paths using the index into the vector.
        #[derive(Serialize)]
        struct FileInfo {
            path: String,
        }

        /// Information about a function name found in one or more backtraces in the
        /// annotated schedule. The name is stored in this type; instances of this
        /// type are stored in the `functions` vector in `AnnotationState`, and
        /// backtrace frames then refer to functions using the index into the vector.
        #[derive(Serialize)]
        struct FunctionInfo {
            name: String,
        }

        /// A backtrace frame.
        #[derive(Serialize)]
        struct Frame(
            // file (index into `state.files`)
            usize,
            // function (index into `state.functions`)
            usize,
            // line
            usize,
            // column
            usize,
        );

        /// Information about a shared object, i.e., a synchronization primitive
        /// based on a batch semaphore.
        #[derive(Serialize)]
        struct ObjectInfo {
            created_by: TaskId,
            created_at: usize,
            name: Option<String>,
            kind: Option<String>,
        }

        /// Information about a task.
        #[derive(Serialize)]
        struct TaskInfo {
            created_by: TaskId,
            first_step: usize,
            last_step: usize,
            name: Option<String>,
        }

        #[derive(Debug, Serialize)]
        enum AnnotationEvent {
            SemaphoreCreated(ObjectId),
            SemaphoreClosed(ObjectId),
            SemaphoreAcquireFast(ObjectId, usize),
            SemaphoreAcquireBlocked(ObjectId, usize),
            SemaphoreAcquireUnblocked(ObjectId, TaskId, usize),
            SemaphoreTryAcquire(ObjectId, usize, bool),
            SemaphoreRelease(ObjectId, usize),

            TaskCreated(TaskId, bool),
            TaskTerminated,

            Random,
            Tick,
        }

        #[derive(Serialize)]
        struct EventInfo(
            // which task did something/yielded?
            TaskId,
            // backtrace
            Option<Vec<Frame>>,
            // event kind
            AnnotationEvent,
            // (if available,) clock of the task
            // TODO: should always be available?
            Option<VectorClock>,
            // which other tasks were available to schedule, if this was a scheduled tick
            Option<Vec<TaskId>>,
        );

        #[derive(Default, Serialize)]
        struct AnnotationState {
            version: usize,
            files: Vec<FileInfo>,
            #[serde(skip)]
            path_to_file: HashMap<String, usize>,
            functions: Vec<FunctionInfo>,
            #[serde(skip)]
            name_to_function: HashMap<String, usize>,
            objects: Vec<ObjectInfo>,
            tasks: Vec<TaskInfo>,
            events: Vec<EventInfo>,

            #[serde(skip)]
            last_runnable_ids: Option<Vec<TaskId>>,
            #[serde(skip)]
            last_task_id: Option<TaskId>,
            #[serde(skip)]
            max_task_id: Option<TaskId>,
        }

        impl Serialize for VectorClock {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::ser::Serializer,
            {
                use serde::ser::SerializeSeq;
                let mut seq = serializer.serialize_seq(Some(self.time.len()))?;
                for e in &self.time {
                    seq.serialize_element(e)?;
                }
                seq.end()
            }
        }

        impl Serialize for TaskId {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::ser::Serializer,
            {
                usize::from(*self).serialize(serializer)
            }
        }

        fn record_event(event: AnnotationEvent) {
            with_state(move |state| {
                let task_id = state.last_task_id.expect("no last task ID");

                let task_id_num = usize::from(task_id);
                assert!(task_id_num < state.tasks.len());
                state.tasks[task_id_num].first_step = state.tasks[task_id_num].first_step.min(state.events.len());
                state.tasks[task_id_num].last_step = state.tasks[task_id_num].last_step.max(state.events.len());

                use std::backtrace::{Backtrace, BacktraceStatus};
                use std::sync::OnceLock;
                use regex::Regex;

                // Here is a fragment of a backtrace for reference:
                // ```
                // 2: core::panicking::assert_failed_inner
                // 3: core::panicking::assert_failed
                //           at /rustc/129f3b9964af4d4a709d1383930ade12dfe7c081/library/core/src/panicking.rs:364:5
                // 4: shuttle_clients::tests::example_impl
                //           at ./src/example.rs:15:5
                // 5: core::ops::function::Fn::call
                //           at /rustc/129f3b9964af4d4a709d1383930ade12dfe7c081/library/core/src/ops/function.rs:79:5
                // ```
                // We want to capture the frames (numbered lines above) which
                // refer to files local to the project being run, as well as
                // the function name, and line/column info.
                // TODO: for now, "local to the project" is detected based on
                //       the path starting with `./src/`. Find an alternative
                //       way to do this?
                // The following regex matches frames with local paths. We rely
                // on the string format of the backtrace because there is no
                // better API. At the time of writing, even the unstable feature
                // `backtrace_frames` does not provide a better interface.
                // https://doc.rust-lang.org/std/backtrace/struct.BacktraceFrame.html
                static RE: OnceLock<Regex> = OnceLock::new();
                //                                         _num      function_name  path           line     col
                let regex = RE.get_or_init(|| Regex::new(r"([0-9]+): ([^\n]+)\n +at (\./src/[^:]+):([0-9]+):([0-9]+)\b").unwrap());

                // Whether or not the following call actually captures a backtrace
                // depends on the environment variables `RUST_BACKTRACE` and
                // `RUST_LIB_BACKTRACE`. See:
                // https://doc.rust-lang.org/std/backtrace/index.html#environment-variables
                // TODO: alternatively, we could use `Backtrace::force_capture`
                //       and use our own environment flag.
                let bt = Backtrace::capture();
                let info = if bt.status() == BacktraceStatus::Captured {
                    Some(regex
                        // apply regex to debug-formatted backtrace
                        .captures_iter(&format!("{bt}"))
                        // for each match, extract the captured groups
                        .map(|group| group.extract().1)
                        // then store the extracted data into a `Frame`
                        .map(|[_num, function_name, path, line_str, col_str]| {
                            // intern file path in `state.files`
                            let path_idx = *state
                                .path_to_file
                                .entry(path.to_string())
                                .or_insert_with(|| {
                                    let idx = state.files.len();
                                    state.files.push(FileInfo {
                                        path: path.to_string(),
                                    });
                                    idx
                                });

                            // intern function name in `state.functions`
                            let function_idx = *state
                                .name_to_function
                                .entry(function_name.to_string())
                                .or_insert_with(|| {
                                    let idx = state.functions.len();
                                    state.functions.push(FunctionInfo {
                                        name: function_name.to_string(),
                                    });
                                    idx
                                });

                            Frame(
                                path_idx,                           // file
                                function_idx,                       // function
                                line_str.parse::<usize>().unwrap(), // line
                                col_str.parse::<usize>().unwrap(),  // col
                            )
                        })
                        .collect::<Vec<_>>())
                } else {
                    None
                };

                state.events.push(EventInfo(
                    task_id,
                    info,
                    event,
                    ExecutionState::try_with(|state| state.get_clock(task_id).clone()),
                    state.last_runnable_ids.take(),
                ))
            });
        }

        fn with_state<R, F: FnOnce(&mut AnnotationState) -> R>(f: F) -> Option<R> {
            ANNOTATION_STATE.with(|cell| {
                let mut bw = cell.borrow_mut();
                let state = bw.as_mut()?;
                Some(f(state))
            })
        }

        fn record_object() -> ObjectId {
            with_state(|state| {
                let id = ObjectId(state.objects.len());
                state.objects.push(ObjectInfo {
                    created_by: state.last_task_id.unwrap(),
                    created_at: state.events.len(),
                    name: None,
                    kind: None,
                });
                id
            })
            .unwrap_or(DUMMY_OBJECT_ID)
        }

        pub(crate) fn start_annotations() {
            ANNOTATION_STATE.with(|cell| {
                let mut bw = cell.borrow_mut();
                assert!(bw.is_none(), "annotations already started");
                let state = AnnotationState {
                    version: ANNOTATION_VERSION,
                    last_task_id: Some(0.into()),
                    ..Default::default()
                };
                *bw = Some(state);
            });
        }

        pub(crate) fn stop_annotations() {
            ANNOTATION_STATE.with(|cell| {
                let mut bw = cell.borrow_mut();
                let state = bw.take().expect("annotations not started");
                if state.max_task_id.is_none() {
                    // nothing to output
                    return;
                };
                let json = serde_json::to_string(&state).unwrap();
                std::fs::write(
                    annotation_file(),
                    json,
                )
                .unwrap();
            });
        }

        pub(crate) fn record_semaphore_created() -> ObjectId {
            let object_id = record_object();
            record_event(AnnotationEvent::SemaphoreCreated(object_id));
            object_id
        }

        pub(crate) fn record_semaphore_closed(object_id: ObjectId) {
            record_event(AnnotationEvent::SemaphoreClosed(object_id));
        }

        pub(crate) fn record_semaphore_acquire_fast(object_id: ObjectId, num_permits: usize) {
            record_event(AnnotationEvent::SemaphoreAcquireFast(object_id, num_permits));
        }

        pub(crate) fn record_semaphore_acquire_blocked(object_id: ObjectId, num_permits: usize) {
            record_event(AnnotationEvent::SemaphoreAcquireBlocked(object_id, num_permits));
        }

        pub(crate) fn record_semaphore_acquire_unblocked(object_id: ObjectId, unblocked_task_id: TaskId, num_permits: usize) {
            record_event(AnnotationEvent::SemaphoreAcquireUnblocked(
                object_id,
                unblocked_task_id,
                num_permits,
            ));
        }

        pub(crate) fn record_semaphore_try_acquire(object_id: ObjectId, num_permits: usize, successful: bool) {
            record_event(AnnotationEvent::SemaphoreTryAcquire(object_id, num_permits, successful));
        }

        pub(crate) fn record_semaphore_release(object_id: ObjectId, num_permits: usize) {
            record_event(AnnotationEvent::SemaphoreRelease(object_id, num_permits));
        }

        pub(crate) fn record_task_created(task_id: TaskId, is_future: bool) {
            with_state(move |state| {
                assert_eq!(state.tasks.len(), usize::from(task_id));
                state.tasks.push(TaskInfo {
                    created_by: state.last_task_id.unwrap(),
                    first_step: usize::MAX,
                    last_step: 0,
                    name: None,
                });
            });
            record_event(AnnotationEvent::TaskCreated(task_id, is_future));
        }

        pub(crate) fn record_task_terminated() {
            record_event(AnnotationEvent::TaskTerminated);
        }

        pub(crate) fn record_name_for_object(object_id: ObjectId, name: Option<&str>, kind: Option<&str>) {
            with_state(move |state| {
                if let Some(object_info) = state.objects.get_mut(object_id.0) {
                    if name.is_some() {
                        object_info.name = name.map(|name| name.to_string());
                    }
                    if kind.is_some() {
                        object_info.kind = kind.map(|kind| kind.to_string());
                    }
                } // TODO: else panic? warn?
            });
        }

        pub(crate) fn record_name_for_task(task_id: TaskId, name: &crate::current::TaskName) {
            with_state(|state| {
                if let Some(task_info) = state.tasks.get_mut(usize::from(task_id)) {
                    let name: &String = name.into();
                    task_info.name = Some(name.to_string());
                } // TODO: else panic? warn?
            });
        }

        pub(crate) fn record_random() {
            record_event(AnnotationEvent::Random);
        }

        pub(crate) fn record_schedule(choice: TaskId, runnable_tasks: &[&Task]) {
            with_state(|state| {
                let choice_id_num = usize::from(choice);
                state.tasks[choice_id_num].first_step = state.tasks[choice_id_num].first_step.min(state.events.len());
                state.tasks[choice_id_num].last_step = state.tasks[choice_id_num].last_step.max(state.events.len());
                assert!(
                    state.last_runnable_ids.is_none(),
                    "multiple schedule calls without a Tick"
                );
                state.last_runnable_ids = Some(runnable_tasks.iter().map(|task| task.id()).collect::<Vec<_>>());
                state.last_task_id = Some(choice);
                state.max_task_id = state.max_task_id.max(Some(choice));
            });
        }

        pub(crate) fn record_tick() {
            record_event(AnnotationEvent::Tick);
        }
    } else {
        use crate::runtime::task::{Task, TaskId};

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) struct ObjectId;

        pub(crate) const DUMMY_OBJECT_ID: ObjectId = ObjectId;

        #[inline(always)]
        pub(crate) fn start_annotations() {}

        #[inline(always)]
        pub(crate) fn stop_annotations() {}

        #[inline(always)]
        pub(crate) fn record_semaphore_created() -> ObjectId {
            DUMMY_OBJECT_ID
        }

        #[inline(always)]
        pub(crate) fn record_semaphore_closed(_object_id: ObjectId) {}

        #[inline(always)]
        pub(crate) fn record_semaphore_acquire_fast(_object_id: ObjectId, _num_permits: usize) {}

        #[inline(always)]
        pub(crate) fn record_semaphore_acquire_blocked(_object_id: ObjectId, _num_permits: usize) {}

        #[inline(always)]
        pub(crate) fn record_semaphore_acquire_unblocked(_object_id: ObjectId, _unblocked_task_id: TaskId, _num_permits: usize) {}

        #[inline(always)]
        pub(crate) fn record_semaphore_try_acquire(_object_id: ObjectId, _num_permits: usize, _successful: bool) {}

        #[inline(always)]
        pub(crate) fn record_semaphore_release(_object_id: ObjectId, _num_permits: usize) {}

        #[inline(always)]
        pub(crate) fn record_task_created(_task_id: TaskId, _future: bool) {}

        #[inline(always)]
        pub(crate) fn record_task_terminated() {}

        #[inline(always)]
        pub(crate) fn record_name_for_object(_object_id: ObjectId, _name: Option<&str>, _kind: Option<&str>) {}

        #[inline(always)]
        pub(crate) fn record_name_for_task(_task_id: TaskId, _name: &crate::current::TaskName) {}

        #[inline(always)]
        pub(crate) fn record_random() {}

        #[inline(always)]
        pub(crate) fn record_schedule(_choice: TaskId, _runnable_tasks: &[&Task]) {}

        #[inline(always)]
        pub(crate) fn record_tick() {}
    }
}

/// Trait to record information about shared objects, such as their name and
/// type. See implementation in [`crate::future::batch_semaphore::BatchSemaphore`], which actually records the
/// name into the schedule, other types should forward calls into their
/// underlying primitive, as in [`crate::sync::Mutex`].
pub trait WithName {
    /// Set the name and kind (full type path) of this object.
    fn with_name_and_kind(self, name: Option<&str>, kind: Option<&str>) -> Self;

    /// Set the name of this object.
    fn with_name(self, name: &str) -> Self
    where
        Self: Sized,
    {
        self.with_name_and_kind(Some(name), None)
    }

    /// Set the kind (full type path) of this object.
    fn with_kind(self, kind: &str) -> Self
    where
        Self: Sized,
    {
        self.with_name_and_kind(None, Some(kind))
    }
}
