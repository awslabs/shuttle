//! API parity: every public item of crossbeam-queue 0.3.14 (read from its source), with its exact
//! signature, written once and checked against crossbeam-queue and against this crate. A difference
//! in a signature, a missing method or a missing impl fails to compile for one of the two.

macro_rules! probe {
    ($name:ident, $krate:ident) => {
        #[allow(dead_code, clippy::type_complexity)]
        mod $name {
            use std::fmt::Debug;
            use std::panic::{RefUnwindSafe, UnwindSafe};
            use $krate::{ArrayQueue, SegQueue};

            fn bounds<Q: Send + Sync + Debug + UnwindSafe + RefUnwindSafe>() {}
            fn iterator<I: Iterator<Item = T> + Send, T>() {}
            fn debug<D: Debug>() {}

            /// For every `T: Send`, as crossbeam's impls are.
            fn impls<T: Send>() {
                bounds::<ArrayQueue<T>>();
                bounds::<SegQueue<T>>();
                iterator::<<ArrayQueue<T> as IntoIterator>::IntoIter, T>();
                iterator::<<SegQueue<T> as IntoIterator>::IntoIter, T>();
                let _: fn() -> SegQueue<T> = <SegQueue<T> as Default>::default;
            }

            /// crossbeam derives its iterators' `Debug`.
            fn debug_impls<T: Send + Debug>() {
                debug::<<ArrayQueue<T> as IntoIterator>::IntoIter>();
                debug::<<SegQueue<T> as IntoIterator>::IntoIter>();
            }

            fn array_queue<T>() {
                let _: fn(usize) -> ArrayQueue<T> = ArrayQueue::<T>::new;
                let _: fn(&ArrayQueue<T>, T) -> Result<(), T> = ArrayQueue::<T>::push;
                let _: fn(&mut ArrayQueue<T>, T) -> Result<(), T> = ArrayQueue::<T>::push_mut;
                let _: fn(&ArrayQueue<T>, T) -> Option<T> = ArrayQueue::<T>::force_push;
                let _: fn(&ArrayQueue<T>) -> Option<T> = ArrayQueue::<T>::pop;
                let _: fn(&mut ArrayQueue<T>) -> Option<T> = ArrayQueue::<T>::pop_mut;
                let _: fn(&ArrayQueue<T>) -> usize = ArrayQueue::<T>::capacity;
                let _: fn(&ArrayQueue<T>) -> bool = ArrayQueue::<T>::is_empty;
                let _: fn(&ArrayQueue<T>) -> bool = ArrayQueue::<T>::is_full;
                let _: fn(&ArrayQueue<T>) -> usize = ArrayQueue::<T>::len;
                let _: fn(ArrayQueue<T>) -> <ArrayQueue<T> as IntoIterator>::IntoIter =
                    <ArrayQueue<T> as IntoIterator>::into_iter;
            }

            fn seg_queue<T>() {
                let _: fn() -> SegQueue<T> = SegQueue::<T>::new;
                let _: fn(&SegQueue<T>, T) = SegQueue::<T>::push;
                let _: fn(&mut SegQueue<T>, T) = SegQueue::<T>::push_mut;
                let _: fn(&SegQueue<T>) -> Option<T> = SegQueue::<T>::pop;
                let _: fn(&mut SegQueue<T>) -> Option<T> = SegQueue::<T>::pop_mut;
                let _: fn(&SegQueue<T>) -> bool = SegQueue::<T>::is_empty;
                let _: fn(&SegQueue<T>) -> usize = SegQueue::<T>::len;
                let _: fn(SegQueue<T>) -> <SegQueue<T> as IntoIterator>::IntoIter =
                    <SegQueue<T> as IntoIterator>::into_iter;
            }

            /// `SegQueue::new` is `const`, so a `SegQueue` can be a `static`.
            static STATIC: SegQueue<String> = SegQueue::new();
        }
    };
}

probe!(crossbeam, crossbeam_queue_orig);
probe!(model, shuttle_crossbeam_queue_inner);

#[test]
fn the_probe_compiles_for_both() {}
