//! API parity: every public item of crossbeam-skiplist 0.1.3's `map` and `set` modules (read from its
//! source), with its exact signature, written once and checked against crossbeam-skiplist and against
//! this crate. A difference in a signature, a missing method or a missing impl fails to compile for
//! one of the two. crossbeam-skiplist's `base` module and `SkipList` are not modelled.

macro_rules! probe {
    ($name:ident, $krate:ident) => {
        #[allow(dead_code, clippy::type_complexity)]
        mod $name {
            use std::borrow::Borrow;
            use std::fmt::Debug;
            use std::ops::{Bound, Deref, RangeBounds};
            use $krate::{SkipMap, SkipSet, map, set};

            fn send_sync<Q: Send + Sync>() {}
            fn debug<Q: Debug>() {}
            fn iterator<I: Iterator<Item = T>, T>() {}
            fn double_ended<I: DoubleEndedIterator<Item = T>, T>() {}

            fn impls<K: Send + Sync + Ord + Debug + 'static, V: Send + Sync + Debug + 'static>() {
                send_sync::<SkipMap<K, V>>();
                send_sync::<SkipSet<K>>();
                send_sync::<map::Entry<'static, K, V>>();
                send_sync::<set::Entry<'static, K>>();
                debug::<SkipMap<K, V>>();
                debug::<SkipSet<K>>();
                debug::<map::Entry<'static, K, V>>();
                debug::<set::Entry<'static, K>>();
                debug::<map::Iter<'static, K, V>>();
                debug::<set::Iter<'static, K>>();
                debug::<map::IntoIter<K, V>>();
                debug::<set::IntoIter<K>>();
                debug::<map::Range<'static, K, std::ops::RangeFull, K, V>>();
                debug::<set::Range<'static, K, std::ops::RangeFull, K>>();
                let _: fn() -> SkipMap<K, V> = <SkipMap<K, V> as Default>::default;
                let _: fn() -> SkipSet<K> = <SkipSet<K> as Default>::default;
                let _: fn(Vec<(K, V)>) -> SkipMap<K, V> = <SkipMap<K, V> as FromIterator<(K, V)>>::from_iter;
                let _: fn(Vec<K>) -> SkipSet<K> = <SkipSet<K> as FromIterator<K>>::from_iter;
                let _: fn(&map::Entry<'static, K, V>) -> map::Entry<'static, K, V> = Clone::clone;
                let _: fn(&set::Entry<'static, K>) -> set::Entry<'static, K> = Clone::clone;
                let _: for<'x> fn(&'x set::Entry<'static, K>) -> &'x K = <set::Entry<'static, K> as Deref>::deref;
                iterator::<map::IntoIter<K, V>, (K, V)>();
                iterator::<set::IntoIter<K>, K>();
                double_ended::<map::Iter<'static, K, V>, map::Entry<'static, K, V>>();
                double_ended::<set::Iter<'static, K>, set::Entry<'static, K>>();
                double_ended::<map::Range<'static, K, std::ops::RangeFull, K, V>, map::Entry<'static, K, V>>();
                double_ended::<set::Range<'static, K, std::ops::RangeFull, K>, set::Entry<'static, K>>();
                let _: fn(SkipMap<K, V>) -> map::IntoIter<K, V> = <SkipMap<K, V> as IntoIterator>::into_iter;
                let _: fn(SkipSet<K>) -> set::IntoIter<K> = <SkipSet<K> as IntoIterator>::into_iter;
                let _: fn(&'static SkipMap<K, V>) -> map::Iter<'static, K, V> =
                    <&'static SkipMap<K, V> as IntoIterator>::into_iter;
                let _: fn(&'static SkipSet<K>) -> set::Iter<'static, K> =
                    <&'static SkipSet<K> as IntoIterator>::into_iter;
            }

            /// The methods without bounds on `K` and `V`.
            fn map_any<'a, K: 'a, V: 'a>() {
                let _: fn() -> SkipMap<K, V> = SkipMap::<K, V>::new;
                let _: fn(&SkipMap<K, V>) -> bool = SkipMap::<K, V>::is_empty;
                let _: fn(&SkipMap<K, V>) -> usize = SkipMap::<K, V>::len;
                let _: for<'x> fn(&'x map::Entry<'a, K, V>) -> &'x K = map::Entry::<'a, K, V>::key;
                let _: for<'x> fn(&'x map::Entry<'a, K, V>) -> &'x V = map::Entry::<'a, K, V>::value;
                let _: fn(&map::Entry<'a, K, V>) -> bool = map::Entry::<'a, K, V>::is_removed;
            }

            fn map_ord<'a, K: Ord + Borrow<Q> + 'a, V: 'a, Q: Ord + ?Sized + 'a, R: RangeBounds<Q> + 'a>() {
                let _: fn(&'a SkipMap<K, V>) -> Option<map::Entry<'a, K, V>> = SkipMap::<K, V>::front;
                let _: fn(&'a SkipMap<K, V>) -> Option<map::Entry<'a, K, V>> = SkipMap::<K, V>::back;
                let _: fn(&SkipMap<K, V>, &Q) -> bool = SkipMap::<K, V>::contains_key::<Q>;
                let _: fn(&'a SkipMap<K, V>, &Q) -> Option<map::Entry<'a, K, V>> = SkipMap::<K, V>::get::<Q>;
                let _: fn(&'a SkipMap<K, V>, Bound<&Q>) -> Option<map::Entry<'a, K, V>> =
                    SkipMap::<K, V>::lower_bound::<Q>;
                let _: fn(&'a SkipMap<K, V>, Bound<&Q>) -> Option<map::Entry<'a, K, V>> =
                    SkipMap::<K, V>::upper_bound::<Q>;
                let _: fn(&'a SkipMap<K, V>, K, V) -> map::Entry<'a, K, V> = SkipMap::<K, V>::get_or_insert;
                let _: fn(&'a SkipMap<K, V>, K, fn() -> V) -> map::Entry<'a, K, V> =
                    SkipMap::<K, V>::get_or_insert_with::<fn() -> V>;
                let _: fn(&'a SkipMap<K, V>) -> map::Iter<'a, K, V> = SkipMap::<K, V>::iter;
                let _: fn(&'a SkipMap<K, V>, R) -> map::Range<'a, Q, R, K, V> = SkipMap::<K, V>::range::<Q, R>;
                let _: fn(&mut map::Entry<'a, K, V>) -> bool = map::Entry::<'a, K, V>::move_next;
                let _: fn(&mut map::Entry<'a, K, V>) -> bool = map::Entry::<'a, K, V>::move_prev;
                let _: fn(&map::Entry<'a, K, V>) -> Option<map::Entry<'a, K, V>> = map::Entry::<'a, K, V>::next;
                let _: fn(&map::Entry<'a, K, V>) -> Option<map::Entry<'a, K, V>> = map::Entry::<'a, K, V>::prev;
            }

            fn map_owned<'a, K: Ord + Send + 'static + Borrow<Q>, V: Send + 'static, Q: Ord + ?Sized + 'a>() {
                let _: fn(&'a SkipMap<K, V>, K, V) -> map::Entry<'a, K, V> = SkipMap::<K, V>::insert;
                let _: fn(&'a SkipMap<K, V>, K, V, fn(&V) -> bool) -> map::Entry<'a, K, V> =
                    SkipMap::<K, V>::compare_insert::<fn(&V) -> bool>;
                let _: fn(&'a SkipMap<K, V>, &Q) -> Option<map::Entry<'a, K, V>> = SkipMap::<K, V>::remove::<Q>;
                let _: fn(&'a SkipMap<K, V>) -> Option<map::Entry<'a, K, V>> = SkipMap::<K, V>::pop_front;
                let _: fn(&'a SkipMap<K, V>) -> Option<map::Entry<'a, K, V>> = SkipMap::<K, V>::pop_back;
                let _: fn(&SkipMap<K, V>) = SkipMap::<K, V>::clear;
                let _: fn(&map::Entry<'a, K, V>) -> bool = map::Entry::<'a, K, V>::remove;
            }

            fn set_any<'a, T: 'a>() {
                let _: fn() -> SkipSet<T> = SkipSet::<T>::new;
                let _: fn(&SkipSet<T>) -> bool = SkipSet::<T>::is_empty;
                let _: fn(&SkipSet<T>) -> usize = SkipSet::<T>::len;
                let _: for<'x> fn(&'x set::Entry<'a, T>) -> &'x T = set::Entry::<'a, T>::value;
                let _: fn(&set::Entry<'a, T>) -> bool = set::Entry::<'a, T>::is_removed;
            }

            fn set_ord<'a, T: Ord + Borrow<Q> + 'a, Q: Ord + ?Sized + 'a, R: RangeBounds<Q> + 'a>() {
                let _: fn(&'a SkipSet<T>) -> Option<set::Entry<'a, T>> = SkipSet::<T>::front;
                let _: fn(&'a SkipSet<T>) -> Option<set::Entry<'a, T>> = SkipSet::<T>::back;
                let _: fn(&SkipSet<T>, &Q) -> bool = SkipSet::<T>::contains::<Q>;
                let _: fn(&'a SkipSet<T>, &Q) -> Option<set::Entry<'a, T>> = SkipSet::<T>::get::<Q>;
                let _: fn(&'a SkipSet<T>, Bound<&Q>) -> Option<set::Entry<'a, T>> = SkipSet::<T>::lower_bound::<Q>;
                let _: fn(&'a SkipSet<T>, Bound<&Q>) -> Option<set::Entry<'a, T>> = SkipSet::<T>::upper_bound::<Q>;
                let _: fn(&'a SkipSet<T>, T) -> set::Entry<'a, T> = SkipSet::<T>::get_or_insert;
                let _: fn(&'a SkipSet<T>) -> set::Iter<'a, T> = SkipSet::<T>::iter;
                let _: fn(&'a SkipSet<T>, R) -> set::Range<'a, Q, R, T> = SkipSet::<T>::range::<Q, R>;
                let _: fn(&mut set::Entry<'a, T>) -> bool = set::Entry::<'a, T>::move_next;
                let _: fn(&mut set::Entry<'a, T>) -> bool = set::Entry::<'a, T>::move_prev;
                let _: fn(&set::Entry<'a, T>) -> Option<set::Entry<'a, T>> = set::Entry::<'a, T>::next;
                let _: fn(&set::Entry<'a, T>) -> Option<set::Entry<'a, T>> = set::Entry::<'a, T>::prev;
            }

            fn set_owned<'a, T: Ord + Send + 'static + Borrow<Q>, Q: Ord + ?Sized + 'a>() {
                let _: fn(&'a SkipSet<T>, T) -> set::Entry<'a, T> = SkipSet::<T>::insert;
                let _: fn(&'a SkipSet<T>, &Q) -> Option<set::Entry<'a, T>> = SkipSet::<T>::remove::<Q>;
                let _: fn(&'a SkipSet<T>) -> Option<set::Entry<'a, T>> = SkipSet::<T>::pop_front;
                let _: fn(&'a SkipSet<T>) -> Option<set::Entry<'a, T>> = SkipSet::<T>::pop_back;
                let _: fn(&SkipSet<T>) = SkipSet::<T>::clear;
                let _: fn(&set::Entry<'a, T>) -> bool = set::Entry::<'a, T>::remove;
            }

            /// The paths consumers name.
            type Paths = (
                map::SkipMap<u8, u8>,
                set::SkipSet<u8>,
                map::Iter<'static, u8, u8>,
                map::Range<'static, u8, std::ops::Range<u8>, u8, u8>,
                map::IntoIter<u8, u8>,
                set::Iter<'static, u8>,
                set::Range<'static, u8, std::ops::Range<u8>, u8>,
                set::IntoIter<u8>,
            );
        }
    };
}

probe!(crossbeam, crossbeam_skiplist_orig);
probe!(model, shuttle_crossbeam_skiplist_inner);

#[test]
fn the_probe_compiles_for_both() {}
