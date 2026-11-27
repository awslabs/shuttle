use std::cell::Cell;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct Sender<T> {
    _p: PhantomData<fn(T) -> T>,
}

#[derive(Debug)]
pub struct Receiver<T> {
    _p: PhantomData<T>,
}

pub(crate) struct Enter<'a, T> {
    _rx: &'a mut Receiver<T>,
    prev: *mut (),
}

// Note: It is considered unsound for anyone other than our macros to call
// this function. This is a private API intended only for calls from our
// macros, and users should never call it, but some people tend to
// misinterpret it as fine to call unless it is marked unsafe.
/// Makes the sender and the receiver of a stream.
///
/// # Safety
///
/// The caller must ensure:
/// * The receiver goes into an `AsyncStream` together with a generator, and the sender is moved
///   into that generator only.
/// * The generator awaits each future that `Sender::send` returns in its own body, and not in a
///   closure, a nested async block or the body of another stream.
///
/// `Send::poll` writes the item into the slot that `STORE` points to. These two properties are how
/// it knows that the slot belongs to its own stream and has its item type. The only callers are the
/// code that the hidden `stream_inner!` and `try_stream_inner!` macros make, for `stream!` and
/// `try_stream!`. Code can also call the hidden macros directly, and then the second property can
/// fail (see `Send::poll`).
#[doc(hidden)]
pub unsafe fn pair<T>() -> (Sender<T>, Receiver<T>) {
    let tx = Sender { _p: PhantomData };
    let rx = Receiver { _p: PhantomData };
    (tx, rx)
}

// Tracks the pointer to `Option<T>`.
//
// TODO: Ensure wakers match?
// SHUTTLE_CHANGES: Importing the thread local from `shuttle` is the only change to the code from
// `async-stream`. The safety comments of `pair` and `Send::poll` are rewritten.
shuttle::thread_local!(static STORE: Cell<*mut ()> = const { Cell::new(ptr::null_mut()) });

// ===== impl Sender =====

impl<T> Sender<T> {
    pub fn send(&mut self, value: T) -> impl Future<Output = ()> {
        Send { value: Some(value) }
    }
}

struct Send<T> {
    value: Option<T>,
}

impl<T> Unpin for Send<T> {}

impl<T> Future for Send<T> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.value.is_none() {
            return Poll::Ready(());
        }

        STORE.with(|cell| {
            let ptr = cell.get() as *mut Option<T>;

            // SAFETY:
            // In order for `as_mut` to be safe, we need to ensure:
            // * `ptr` is null, or it points to a live `Option<T>`, with the `T` of this future.
            // * Nothing else reads or writes that `Option<T>` while `option_ref` is in use.
            //
            // `STORE` is null unless `Receiver::enter` set it. `AsyncStream::poll_next` calls
            // `enter` with its local `dst`, polls the generator, and then drops the `Enter`, which
            // puts the old pointer back, also if the poll panics. An inner stream that a generator
            // polls does the same before its poll returns. So while a generator is polled, `STORE`
            // points to the `dst` of the innermost stream whose poll is running, and that `dst` is
            // alive on the stack of that poll.
            // `STORE` is a Shuttle thread-local, so each Shuttle task has its own. A context switch
            // in the middle of a poll lets another task poll its own streams, but they change only
            // that task's `STORE`. A `std` thread-local would be shared by all the tasks.
            // `pair` makes this future's `Sender<T>` together with the `Receiver<T>` of its stream,
            // whose `dst` is an `Option<T>`. The `# Safety` section of `pair` asks that this future
            // is awaited directly in the body of that stream's generator. The `stream!` and
            // `try_stream!` macros do that. A `yield` in a closure or a nested async block fails to
            // compile: the macros leave such a `yield` as it is, and next to each `yield` that they
            // turn into a send, they add a labeled `break`, which can't reach its label from inside
            // a closure or an async block. async-stream-impl adds this `break` from 0.3.6, which
            // `Cargo.toml` asks for. `try_stream!` also turns a `?` into a send. The macros change a
            // `?` only outside closures, async blocks and other macros, so those sends are in the
            // body itself too. The body can't name the sender, `__yield_tx`, because of
            // `macro_rules!` hygiene. So when this future is polled, the innermost stream is its
            // own, and `ptr` has the right type.
            // This does not hold for code that calls the hidden `__private::stream_inner!` or
            // `try_stream_inner!` macros directly, because their bodies can name `__yield_tx`. A
            // send future that such a body makes can be awaited in the body of an inner stream,
            // and then `ptr` points to the `dst` of the inner stream, whose item type can be
            // different. `async-stream` has the same problem.
            // `poll_next` gives `&mut dst` to `enter`, and the returned `Enter` keeps it borrowed
            // until it is dropped. So `poll_next` doesn't use `dst` while the generator is polled,
            // and `option_ref` is used only inside this closure.
            let option_ref = unsafe { ptr.as_mut() };
            let option_ref = option_ref.expect("invalid usage");

            if option_ref.is_none() {
                *option_ref = self.value.take();
            }

            Poll::Pending
        })
    }
}

// ===== impl Receiver =====

impl<T> Receiver<T> {
    pub(crate) fn enter<'a>(&'a mut self, dst: &'a mut Option<T>) -> Enter<'a, T> {
        let prev = STORE.with(|cell| {
            let prev = cell.get();
            cell.set(dst as *mut _ as *mut ());
            prev
        });

        Enter { _rx: self, prev }
    }
}

// ===== impl Enter =====

impl<T> Drop for Enter<'_, T> {
    fn drop(&mut self) {
        STORE.with(|cell| cell.set(self.prev));
    }
}
