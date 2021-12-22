use futures::{future::FutureExt, join, pin_mut, select};
use shuttle::{asynch, check_dfs};
use std::{
    cell::RefCell,
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use test_log::test;

pub(crate) struct CountdownTimerState<T> {
    remain: u32,
    value: Option<T>,
}

pub(crate) struct CountdownTimer<T> {
    state: RefCell<CountdownTimerState<T>>,
}

impl<T> CountdownTimer<T> {
    pub fn new(remain: u32, value: T) -> Self {
        let state = RefCell::new(CountdownTimerState {
            remain,
            value: Some(value),
        });
        Self { state }
    }
}

impl<T> Future for CountdownTimer<T>
where
    T: Debug,
{
    type Output = T;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut cell = self.state.borrow_mut();
        if cell.remain > 0 {
            cell.remain -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(cell.value.take().unwrap())
        }
    }
}

#[test]
fn timer_simple() {
    check_dfs(
        || {
            // Start 3 timers with delays 1,2,3 and values 10,20,40
            let timera = CountdownTimer::new(1, 10);
            let timerb = CountdownTimer::new(2, 20);
            let timerc = CountdownTimer::new(3, 40);
            let v1 = asynch::spawn(timera); // no need for async block
            let v2 = asynch::spawn(async move { timerb.await });
            let v3 = asynch::spawn(async move { timerc.await });
            // Spawn another task that waits for the timers and checks the return values
            asynch::block_on(async move {
                let sum = v1.await.unwrap() + v2.await.unwrap() + v3.await.unwrap();
                assert_eq!(sum, 10 + 20 + 40);
            });
        },
        None,
    );
}

#[test]
fn timer_block_on() {
    check_dfs(
        || {
            let timer_1 = CountdownTimer::new(1, 10);
            let timer_2 = CountdownTimer::new(5, 20);
            let v1 = asynch::block_on(timer_1);
            let v2 = asynch::block_on(timer_2);
            assert_eq!(v1 + v2, 10 + 20);
        },
        None,
    );
}

#[test]
fn timer_select() {
    check_dfs(
        || {
            let timer1 = CountdownTimer::new(10, 10).fuse();
            let timer2 = CountdownTimer::new(20, 20).fuse();
            let timer3 = CountdownTimer::new(30, 30).fuse();
            let r = asynch::block_on(async {
                pin_mut!(timer1, timer2, timer3);
                select! {
                    v1 = timer1 => v1,
                    v2 = timer2 => v2,
                    v3 = timer3 => v3,
                }
            });
            assert_eq!(r, 10);
        },
        None,
    );
}

#[test]
fn timer_join() {
    check_dfs(
        || {
            let timer1 = CountdownTimer::new(10, 10);
            let timer2 = CountdownTimer::new(20, 20);
            let timer3 = CountdownTimer::new(30, 30);
            let (v1, v2, v3) = asynch::block_on(async { join!(timer1, timer2, timer3) });
            assert_eq!(v1 + v2 + v3, 60);
        },
        None,
    );
}
