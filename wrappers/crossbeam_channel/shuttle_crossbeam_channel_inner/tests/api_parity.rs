//! The model has crossbeam-channel's API: the same items with the same signatures, written once
//! and compiled against both crates. Only the `Instant` differs: `std::time`'s for crossbeam,
//! `shuttle-time`'s for the model.
//!
//! This test only has to compile, apart from a run of the selections in both modes.

macro_rules! signatures {
    ($cc:ident, $time:path) => {
        #[allow(dead_code, clippy::type_complexity)]
        pub mod probe {
            use std::fmt::Debug;
            use std::iter::FusedIterator;
            use std::panic::{RefUnwindSafe, UnwindSafe};
            use $cc::{
                IntoIter, Iter, ReadyTimeoutError, Receiver, RecvError, RecvTimeoutError, Select, SelectTimeoutError,
                SelectedOperation, SendError, SendTimeoutError, Sender, TryIter, TryReadyError, TryRecvError,
                TrySelectError, TrySendError,
            };
            use $time::{Duration, Instant};

            fn traits<T: Send + Sync + UnwindSafe + RefUnwindSafe + Clone + Debug>() {}
            fn fused<T: FusedIterator>() {}

            pub fn functions() {
                let _: fn() -> (Sender<u8>, Receiver<u8>) = $cc::unbounded::<u8>;
                let _: fn(usize) -> (Sender<u8>, Receiver<u8>) = $cc::bounded::<u8>;
                let _: fn(Duration) -> Receiver<Instant> = $cc::after;
                let _: fn(Instant) -> Receiver<Instant> = $cc::at;
                let _: fn() -> Receiver<u8> = $cc::never::<u8>;
                let _: fn(Duration) -> Receiver<Instant> = $cc::tick;
                const _NEVER: Receiver<u8> = $cc::never();
            }

            pub fn sender() {
                let _: fn(&Sender<u8>, u8) -> Result<(), TrySendError<u8>> = Sender::try_send;
                let _: fn(&Sender<u8>, u8) -> Result<(), SendError<u8>> = Sender::send;
                let _: fn(&Sender<u8>, u8, Duration) -> Result<(), SendTimeoutError<u8>> = Sender::send_timeout;
                let _: fn(&Sender<u8>, u8, Instant) -> Result<(), SendTimeoutError<u8>> = Sender::send_deadline;
                let _: fn(&Sender<u8>) -> bool = Sender::is_empty;
                let _: fn(&Sender<u8>) -> bool = Sender::is_full;
                let _: fn(&Sender<u8>) -> usize = Sender::len;
                let _: fn(&Sender<u8>) -> Option<usize> = Sender::capacity;
                let _: fn(&Sender<u8>, &Sender<u8>) -> bool = Sender::same_channel;
                traits::<Sender<u8>>();
            }

            pub fn receiver() {
                let _: fn(&Receiver<u8>) -> Result<u8, TryRecvError> = Receiver::try_recv;
                let _: fn(&Receiver<u8>) -> Result<u8, RecvError> = Receiver::recv;
                let _: fn(&Receiver<u8>, Duration) -> Result<u8, RecvTimeoutError> = Receiver::recv_timeout;
                let _: fn(&Receiver<u8>, Instant) -> Result<u8, RecvTimeoutError> = Receiver::recv_deadline;
                let _: fn(&Receiver<u8>) -> bool = Receiver::is_empty;
                let _: fn(&Receiver<u8>) -> bool = Receiver::is_full;
                let _: fn(&Receiver<u8>) -> usize = Receiver::len;
                let _: fn(&Receiver<u8>) -> Option<usize> = Receiver::capacity;
                let _: for<'a> fn(&'a Receiver<u8>) -> Iter<'a, u8> = Receiver::iter;
                let _: for<'a> fn(&'a Receiver<u8>) -> TryIter<'a, u8> = Receiver::try_iter;
                let _: fn(&Receiver<u8>, &Receiver<u8>) -> bool = Receiver::same_channel;
                let _: fn(Receiver<u8>) -> IntoIter<u8> = <Receiver<u8> as IntoIterator>::into_iter;
                fn by_ref<'a>(r: &'a Receiver<u8>) -> Iter<'a, u8> {
                    r.into_iter()
                }
                traits::<Receiver<u8>>();
                fused::<Iter<'static, u8>>();
                fused::<IntoIter<u8>>();
                let _: fn(&mut TryIter<'static, u8>) -> Option<u8> = <TryIter<'static, u8> as Iterator>::next;
            }

            pub fn selection<'a>(s: &'a Sender<u8>, r: &'a Receiver<u8>) {
                let mut sel: Select<'a> = Select::new();
                let mut biased: Select<'a> = Select::new_biased();
                let _: Select<'a> = Select::default();
                let _: Select<'a> = sel.clone();
                let i: usize = sel.send(s);
                let j: usize = sel.recv(r);
                biased.recv(r);
                let _: Result<SelectedOperation<'a>, TrySelectError> = biased.try_select().map_err(|e| e);
                let _: Result<usize, TryReadyError> = biased.try_ready();
                let oper: SelectedOperation<'a> = sel.select();
                let index: usize = oper.index();
                if index == i {
                    let _: Result<(), SendError<u8>> = oper.send(s, 1);
                } else {
                    let _: Result<u8, RecvError> = oper.recv(r);
                }
                let timed: Result<SelectedOperation<'a>, SelectTimeoutError> =
                    sel.select_timeout(Duration::from_millis(1));
                if let Ok(oper) = timed {
                    if oper.index() == i {
                        let _ = oper.send(s, 2);
                    } else {
                        let _ = oper.recv(r);
                    }
                }
                let _: Result<usize, ReadyTimeoutError> = sel.ready_timeout(Duration::from_millis(1));
                let _: usize = sel.ready();
                sel.remove(j);
                let _: fn(&mut Select<'a>, Instant) -> Result<SelectedOperation<'a>, SelectTimeoutError> =
                    Select::select_deadline;
                let _: fn(&mut Select<'a>, Instant) -> Result<usize, ReadyTimeoutError> = Select::ready_deadline;
                fn debug<T: Debug>() {}
                debug::<Select<'a>>();
                debug::<SelectedOperation<'a>>();
            }

            /// Every form of a `select!` case.
            pub fn macros(s: &Sender<u8>, r: &Receiver<u8>) -> u8 {
                use $cc::{select, select_biased};
                let a = select! {
                    recv(r) -> msg => msg.unwrap_or(0),
                    send(s, 1) -> res => { res.unwrap(); 1 }
                };
                let b = select_biased! {
                    recv(r) -> msg => msg.unwrap_or(0),
                    default => 2,
                };
                let c = select! {
                    send(s, 3) -> _res => 3,
                    default(Duration::from_millis(1)) => 4,
                };
                let d = select! {
                    recv(r) -> msg => msg.unwrap_or(0),
                };
                a + b + c + d
            }

            pub fn errors() {
                let _: fn(SendError<u8>) -> u8 = SendError::into_inner;
                let _: fn(&TrySendError<u8>) -> bool = TrySendError::is_full;
                let _: fn(&SendTimeoutError<u8>) -> bool = SendTimeoutError::is_timeout;
                let _: fn(&TryRecvError) -> bool = TryRecvError::is_empty;
                let _: fn(&RecvTimeoutError) -> bool = RecvTimeoutError::is_timeout;
                let _ = (
                    TrySelectError,
                    SelectTimeoutError,
                    TryReadyError,
                    ReadyTimeoutError,
                    RecvError,
                );
            }
        }
    };
}

mod real {
    signatures!(crossbeam_channel, std::time);
}

mod model {
    signatures!(shuttle_crossbeam_channel_inner, shuttle_time_inner::time);
}

// The selections run, in both modes.
#[test]
fn the_probe_runs_on_crossbeam() {
    let (s, r) = crossbeam_channel::unbounded();
    s.send(0).unwrap();
    real::probe::functions();
    real::probe::macros(&s, &r);
}

#[test]
fn the_probe_runs_under_shuttle() {
    shuttle::check_random(
        || {
            let (s, r) = shuttle_crossbeam_channel_inner::unbounded();
            s.send(0).unwrap();
            model::probe::functions();
            model::probe::macros(&s, &r);
        },
        10,
    );
}
