//! API parity: every public item of crossbeam-deque 0.8.8 (read from its source), with its exact
//! signature, written once and checked against crossbeam-deque and against this crate. A difference
//! in a signature, a missing method or a missing impl fails to compile for one of the two.

macro_rules! probe {
    ($name:ident, $krate:ident) => {
        #[allow(dead_code, clippy::type_complexity)]
        mod $name {
            use std::fmt::Debug;
            use $krate::{Injector, Steal, Stealer, Worker};

            fn send<Q: Send + Debug>() {}
            fn send_sync<Q: Send + Sync + Debug>() {}

            /// For every `T: Send`, as crossbeam's impls are.
            fn impls<T: Send>() {
                send::<Worker<T>>();
                send_sync::<Stealer<T>>();
                send_sync::<Injector<T>>();
                let _: fn(&Stealer<T>) -> Stealer<T> = <Stealer<T> as Clone>::clone;
                let _: fn() -> Injector<T> = <Injector<T> as Default>::default;
            }

            fn worker<T>() {
                let _: fn() -> Worker<T> = Worker::<T>::new_fifo;
                let _: fn() -> Worker<T> = Worker::<T>::new_lifo;
                let _: fn(&Worker<T>) -> Stealer<T> = Worker::<T>::stealer;
                let _: fn(&Worker<T>) -> bool = Worker::<T>::is_empty;
                let _: fn(&Worker<T>) -> usize = Worker::<T>::len;
                let _: fn(&Worker<T>, T) = Worker::<T>::push;
                let _: fn(&Worker<T>) -> Option<T> = Worker::<T>::pop;
            }

            fn stealer<T>() {
                let _: fn(&Stealer<T>) -> bool = Stealer::<T>::is_empty;
                let _: fn(&Stealer<T>) -> usize = Stealer::<T>::len;
                let _: fn(&Stealer<T>) -> Steal<T> = Stealer::<T>::steal;
                let _: fn(&Stealer<T>, &Worker<T>) -> Steal<()> = Stealer::<T>::steal_batch;
                let _: fn(&Stealer<T>, &Worker<T>, usize) -> Steal<()> = Stealer::<T>::steal_batch_with_limit;
                let _: fn(&Stealer<T>, &Worker<T>) -> Steal<T> = Stealer::<T>::steal_batch_and_pop;
                let _: fn(&Stealer<T>, &Worker<T>, usize) -> Steal<T> = Stealer::<T>::steal_batch_with_limit_and_pop;
            }

            fn injector<T>() {
                let _: fn() -> Injector<T> = Injector::<T>::new;
                let _: fn(&Injector<T>, T) = Injector::<T>::push;
                let _: fn(&Injector<T>) -> Steal<T> = Injector::<T>::steal;
                let _: fn(&Injector<T>, &Worker<T>) -> Steal<()> = Injector::<T>::steal_batch;
                let _: fn(&Injector<T>, &Worker<T>, usize) -> Steal<()> = Injector::<T>::steal_batch_with_limit;
                let _: fn(&Injector<T>, &Worker<T>) -> Steal<T> = Injector::<T>::steal_batch_and_pop;
                let _: fn(&Injector<T>, &Worker<T>, usize) -> Steal<T> = Injector::<T>::steal_batch_with_limit_and_pop;
                let _: fn(&Injector<T>) -> bool = Injector::<T>::is_empty;
                let _: fn(&Injector<T>) -> usize = Injector::<T>::len;
            }

            /// `Steal` is crossbeam-deque's own type in both, so its API is the same by construction.
            fn steal<T: Copy + Eq>() {
                let _: fn(&Steal<T>) -> bool = Steal::<T>::is_empty;
                let _: fn(Steal<T>) -> Option<T> = Steal::<T>::success;
                let _: Steal<T> = [Steal::Empty, Steal::Retry].into_iter().collect();
            }
        }
    };
}

probe!(crossbeam, crossbeam_deque_orig);
probe!(model, shuttle_crossbeam_deque_inner);

#[test]
fn steal_is_crossbeams_own_type() {
    assert_eq!(
        std::any::TypeId::of::<shuttle_crossbeam_deque_inner::Steal<u8>>(),
        std::any::TypeId::of::<crossbeam_deque_orig::Steal<u8>>()
    );
}
