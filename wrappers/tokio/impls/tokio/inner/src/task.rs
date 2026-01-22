use crate::runtime::Handle;
use pin_project::pin_project;
use shuttle::current::remove_label_for_task;
use shuttle::current::{me, set_label_for_task, ChildLabelFn, TaskName};
use shuttle::future::spawn_local;
use shuttle::scheduler::TaskId;
use std::any::Any;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

pub use shuttle::current::TaskId as Id;
pub use shuttle::future::yield_now;

#[doc(hidden)]
#[deprecated = "Moved to shuttle_tokio_impl_inner::task::coop::consume_budget"]
pub use coop::consume_budget;
#[doc(hidden)]
#[deprecated = "Moved to shuttle_tokio_impl_inner::task::coop::unconstrained"]
pub use coop::unconstrained;
#[doc(hidden)]
#[deprecated = "Moved to shuttle_tokio_impl_inner::task::coop::Unconstrained"]
pub use coop::Unconstrained;

// TODO: Implement. Only exists in order to get compilation to pass, should not actually be used.
pub mod futures {
    pub use tokio::task::futures::TaskLocalFuture;
}

/// Returns the [`Id`] of the currently running task.
pub fn id() -> Id {
    shuttle::current::get_current_task().unwrap()
}

/// Returns the [`Id`] of the currently running task, or `None` if called outside
/// of a task.
pub fn try_id() -> Option<Id> {
    shuttle::current::get_current_task()
}

/// Spawns a future onto the runtime
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let rt = crate::runtime::Handle::current();
    rt.spawn(future)
}

// TODO: See comment in `runtime::Handle::spawn_blocking`
pub fn spawn_blocking<F, R>(func: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let rt = crate::runtime::Handle::current();
    rt.spawn_blocking(func)
}

/// A wrapper around the `JoinHandle` found in Shuttle in order to implement the full `JoinError` API.
#[derive(Debug)]
#[pin_project]
pub struct JoinHandle<T> {
    #[pin]
    inner: shuttle::future::JoinHandle<T>,
}

impl<T> JoinHandle<T> {
    pub(crate) fn new(inner: shuttle::future::JoinHandle<T>) -> Self {
        Self { inner }
    }

    pub fn abort(&self) {
        self.inner.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle {
            inner: self.inner.abort_handle(),
        }
    }
}

/// Task failed to execute to completion.
#[derive(Debug)]
pub struct JoinError {
    repr: Repr,
}

impl Display for JoinError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.repr.fmt(f)
    }
}

impl Error for JoinError {}

// NOTE: Remember to reimplement `Drop` here if this is ever fully moved out of Shuttle.

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.inner.poll(cx).map_err(std::convert::Into::into)
    }
}

impl From<shuttle::future::JoinError> for JoinError {
    fn from(error: shuttle::future::JoinError) -> Self {
        match error {
            shuttle::future::JoinError::Cancelled => Self { repr: Repr::Cancelled },
        }
    }
}

impl JoinError {
    /// Returns true if the error was caused by the task being cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(self.repr, Repr::Cancelled)
    }

    /// Returns true if the error was caused by the task panicking.
    pub fn is_panic(&self) -> bool {
        matches!(self.repr, Repr::Panic)
    }

    pub fn into_panic(self) -> Box<dyn Any + Send + 'static> {
        unimplemented!()
    }

    pub fn try_into_panic(self) -> Result<Box<dyn Any + Send + 'static>, JoinError> {
        unimplemented!()
    }

    pub fn id(&self) -> Id {
        unimplemented!()
    }
}

#[allow(unused)]
#[derive(Debug)]
enum Repr {
    /// Task was aborted
    Cancelled,
    /// Task panicked
    Panic,
}

impl Display for Repr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Repr::Cancelled => write!(f, "task was cancelled"),
            Repr::Panic => write!(f, "task panicked"),
        }
    }
}

pub mod coop {
    use pin_project::pin_project;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Future for the [`unconstrained`](unconstrained) method.
    #[must_use = "Unconstrained does nothing unless polled"]
    #[pin_project]
    pub struct Unconstrained<F> {
        #[pin]
        inner: F,
    }

    impl<F> Future for Unconstrained<F>
    where
        F: Future,
    {
        type Output = <F as Future>::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let inner = self.project().inner;
            inner.poll(cx)
        }
    }

    /// Under Shuttle this is always a thin wrapper around the `inner` which just
    /// forwards the `poll.`
    pub fn unconstrained<F>(inner: F) -> Unconstrained<F> {
        Unconstrained { inner }
    }

    /// Under Shuttle this is always just a `yield_now`.
    pub async fn consume_budget() {
        shuttle::future::yield_now().await;
    }

    /// Not faithfully modelled; always returns `true`.
    /// Always returning `false` would be equally correct. The reason for always returning
    /// `true` is that an implementation which follows the "do work until done or budget exhausted"
    /// becomes "do work until done" with `true` and "do work once" with `false`. Both versions would
    /// require additional implementation to readd the "budget exhausted" logic, but the `false` case
    /// would also require an override of `has_budged_remaining` to readd the "do work until done" part.
    pub fn has_budget_remaining() -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct AbortHandle {
    inner: shuttle::future::AbortHandle,
}

impl Drop for AbortHandle {
    fn drop(&mut self) {}
}

impl AbortHandle {
    pub fn abort(&self) {
        self.inner.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }
}

pub use join_set::JoinSet;

pub mod join_set {
    use crate::task::{AbortHandle, Handle, JoinError, JoinHandle};
    use ::futures::stream::{FuturesUnordered, StreamExt};
    use std::fmt;
    use std::future::Future;

    pub struct JoinSet<T> {
        inner: FuturesUnordered<JoinHandle<T>>,
    }

    #[must_use = "builders do nothing unless used to spawn a task"]
    pub struct Builder<'a, T> {
        joinset: &'a mut JoinSet<T>,
        builder: super::Builder<'a>,
    }

    impl<T> JoinSet<T> {
        pub fn build_task(&mut self) -> Builder<'_, T> {
            Builder {
                builder: super::Builder::new(),
                joinset: self,
            }
        }

        /// Create a new `JoinSet`.
        pub fn new() -> Self {
            Self {
                inner: FuturesUnordered::new(),
            }
        }

        /// Returns the number of tasks currently in the `JoinSet`.
        pub fn len(&self) -> usize {
            self.inner.len()
        }

        /// Returns whether the `JoinSet` is empty.
        pub fn is_empty(&self) -> bool {
            self.inner.is_empty()
        }
    }

    impl<T> fmt::Debug for JoinSet<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("JoinSet").field("len", &self.len()).finish()
        }
    }

    impl<T> Drop for JoinSet<T> {
        fn drop(&mut self) {
            for jh in &self.inner {
                jh.abort();
            }
        }
    }

    impl<T> Default for JoinSet<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: 'static> JoinSet<T> {
        /// Spawn the provided task on the `JoinSet`, returning an [`AbortHandle`]
        /// that can be used to remotely cancel the task.
        ///
        /// The provided future will start running in the background immediately
        /// when this method is called, even if you don't await anything on this
        /// `JoinSet`.
        ///
        /// # Panics
        ///
        /// This method panics if called outside of a Tokio runtime.
        ///
        /// [`AbortHandle`]: crate::task::AbortHandle
        #[track_caller]
        pub fn spawn<F>(&mut self, task: F) -> AbortHandle
        where
            F: Future<Output = T>,
            F: Send + 'static,
            T: Send,
        {
            self.insert(crate::spawn(task))
        }

        #[track_caller]
        pub fn spawn_on<F>(&mut self, task: F, handle: &Handle) -> AbortHandle
        where
            F: Future<Output = T>,
            F: Send + 'static,
            T: Send,
        {
            self.insert(handle.spawn(task))
        }

        #[track_caller]
        pub fn spawn_local<F>(&mut self, task: F) -> AbortHandle
        where
            F: Future<Output = T>,
            F: 'static,
        {
            self.insert(JoinHandle::new(crate::task::spawn_local(task)))
        }

        fn insert(&mut self, jh: JoinHandle<T>) -> AbortHandle {
            let abort = jh.abort_handle();
            self.inner.push(jh);

            abort
        }

        pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
            self.inner.next().await
        }
    }

    impl<T, F> std::iter::FromIterator<F> for JoinSet<T>
    where
        F: Future<Output = T>,
        F: Send + 'static,
        T: Send + 'static,
    {
        fn from_iter<I: IntoIterator<Item = F>>(iter: I) -> Self {
            let mut set = Self::new();
            iter.into_iter().for_each(|task| {
                set.spawn(task);
            });
            set
        }
    }

    impl<'a, T: 'static> Builder<'a, T> {
        /// Assigns a name to the task which will be spawned.
        pub fn name(self, name: &'a str) -> Self {
            let builder = self.builder.name(name);
            Self { builder, ..self }
        }

        /// Spawn the provided task with this builder's settings and store it in the
        /// [`JoinSet`], returning an [`AbortHandle`] that can be used to remotely
        /// cancel the task.
        ///
        /// # Returns
        ///
        /// An [`AbortHandle`] that can be used to remotely cancel the task.
        ///
        /// # Panics
        ///
        /// This method panics if called outside of a Tokio runtime.
        ///
        /// [`AbortHandle`]: crate::task::AbortHandle
        #[track_caller]
        pub fn spawn<F>(self, future: F) -> std::io::Result<AbortHandle>
        where
            F: Future<Output = T>,
            F: Send + 'static,
            T: Send,
        {
            Ok(self.joinset.insert(self.builder.spawn(future)?))
        }

        /// Spawn the provided task on the provided [runtime handle] with this
        /// builder's settings, and store it in the [`JoinSet`].
        ///
        /// # Returns
        ///
        /// An [`AbortHandle`] that can be used to remotely cancel the task.
        ///
        ///
        /// [`AbortHandle`]: crate::task::AbortHandle
        /// [runtime handle]: crate::runtime::Handle
        #[track_caller]
        pub fn spawn_on<F>(self, future: F, handle: &Handle) -> std::io::Result<AbortHandle>
        where
            F: Future<Output = T>,
            F: Send + 'static,
            T: Send,
        {
            Ok(self.joinset.insert(self.builder.spawn_on(future, handle)?))
        }

        /// Spawn the blocking code on the blocking threadpool with this builder's
        /// settings, and store it in the [`JoinSet`].
        ///
        /// # Returns
        ///
        /// An [`AbortHandle`] that can be used to remotely cancel the task.
        ///
        /// # Panics
        ///
        /// This method panics if called outside of a Tokio runtime.
        ///
        /// [`JoinSet`]: crate::task::JoinSet
        /// [`AbortHandle`]: crate::task::AbortHandle
        #[track_caller]
        pub fn spawn_blocking<F>(self, f: F) -> std::io::Result<AbortHandle>
        where
            F: FnOnce() -> T,
            F: Send + 'static,
            T: Send,
        {
            Ok(self.joinset.insert(self.builder.spawn_blocking(f)?))
        }

        /// Spawn the blocking code on the blocking threadpool of the provided
        /// runtime handle with this builder's settings, and store it in the
        /// [`JoinSet`].
        ///
        /// # Returns
        ///
        /// An [`AbortHandle`] that can be used to remotely cancel the task.
        ///
        /// [`JoinSet`]: crate::task::JoinSet
        /// [`AbortHandle`]: crate::task::AbortHandle
        #[track_caller]
        pub fn spawn_blocking_on<F>(self, f: F, handle: &Handle) -> std::io::Result<AbortHandle>
        where
            F: FnOnce() -> T,
            F: Send + 'static,
            T: Send,
        {
            Ok(self.joinset.insert(self.builder.spawn_blocking_on(f, handle)?))
        }
    }

    // Manual `Debug` impl so that `Builder` is `Debug` regardless of whether `T` is
    // `Debug`.
    impl<'a, T> fmt::Debug for Builder<'a, T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("join_set::Builder")
                .field("joinset", &self.joinset)
                .field("builder", &self.builder)
                .finish()
        }
    }
}

// `Builder` is unstable in Tokio
#[derive(Default, Debug)]
pub struct Builder<'a> {
    name: Option<&'a str>,
}

/// Clears the `ChildLabelFn` label for the `task_id` on drop. Used so that we don't keep naming subsequent spawns as well.
struct RestoreChildLabelFnOnDrop {
    task_id: TaskId,
    child_fn: Option<ChildLabelFn>,
}

impl Drop for RestoreChildLabelFnOnDrop {
    fn drop(&mut self) {
        if let Some(func) = &self.child_fn {
            set_label_for_task(self.task_id, func.clone());
        } else {
            remove_label_for_task::<ChildLabelFn>(self.task_id);
        }
    }
}

impl<'a> Builder<'a> {
    /// Creates a new task builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Assigns a name to the task which will be spawned.
    pub fn name(&self, name: &'a str) -> Self {
        Self { name: Some(name) }
    }

    /// Makes the next spawned tasks become named if there is no current `ChildLabelFnOnDrop`.
    /// If the next task will be named, a `Some(RestoreChildLabelFnOnDrop)` will be returned such that only the next spawn is named.
    #[must_use]
    fn handle_naming(&self) -> Option<RestoreChildLabelFnOnDrop> {
        let me = me();

        if let Some(name) = self.name {
            let old_fn = remove_label_for_task::<ChildLabelFn>(me);
            let old_fn_cloned = old_fn.clone();
            let name = TaskName::from(name);
            set_label_for_task(
                me,
                #[allow(clippy::arc_with_non_send_sync)]
                ChildLabelFn(Arc::new(move |task_id, labels| {
                    // If there is a `ChildLabelFn` set, then execute that.
                    if let Some(func) = &old_fn_cloned {
                        func.0(task_id, labels);
                    }

                    // Update the name
                    labels.insert(name.clone());
                })),
            );

            // If we have set the `ChildLabelFn`, then we should restore it after. If not then we would name any subsequent spawns as well.
            Some(RestoreChildLabelFnOnDrop {
                task_id: me,
                child_fn: old_fn,
            })
        } else {
            None
        }
    }

    #[track_caller]
    pub fn spawn<Fut>(self, future: Fut) -> io::Result<JoinHandle<Fut::Output>>
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        let handle = Handle::current();
        self.spawn_on(future, &handle)
    }

    /// Spawn a task with this builder's settings on the provided [runtime
    /// handle].
    ///
    /// See [`Handle::spawn`] for more details.
    ///
    /// [runtime handle]: crate::runtime::Handle
    /// [`Handle::spawn`]: crate::runtime::Handle::spawn
    #[track_caller]
    pub fn spawn_on<Fut>(self, future: Fut, handle: &Handle) -> io::Result<JoinHandle<Fut::Output>>
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        let _drop_guard = self.handle_naming();
        Ok(handle.spawn(future))
    }

    /// Spawns blocking code on the blocking threadpool.
    ///
    /// # Panics
    ///
    /// This method panics if called outside of a Tokio runtime.
    ///
    /// See [`task::spawn_blocking`](crate::task::spawn_blocking)
    /// for more details.
    #[track_caller]
    pub fn spawn_blocking<Function, Output>(self, function: Function) -> io::Result<JoinHandle<Output>>
    where
        Function: FnOnce() -> Output + Send + 'static,
        Output: Send + 'static,
    {
        let handle = Handle::current();
        self.spawn_blocking_on(function, &handle)
    }

    /// Spawns blocking code on the provided [runtime handle]'s blocking threadpool.
    ///
    /// See [`Handle::spawn_blocking`] for more details.
    ///
    /// [runtime handle]: crate::runtime::Handle
    /// [`Handle::spawn_blocking`]: crate::runtime::Handle::spawn_blocking
    #[track_caller]
    pub fn spawn_blocking_on<Function, Output>(
        self,
        function: Function,
        handle: &Handle,
    ) -> io::Result<JoinHandle<Output>>
    where
        Function: FnOnce() -> Output + Send + 'static,
        Output: Send + 'static,
    {
        let _drop_guard = self.handle_naming();
        Ok(handle.spawn_blocking(function))
    }
}
