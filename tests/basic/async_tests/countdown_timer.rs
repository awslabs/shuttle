use shuttle::{asynch, check};
use std::{
    cell::RefCell,
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

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
            cx.waker().clone().wake();
            Poll::Pending
        } else {
            Poll::Ready(cell.value.take().unwrap())
        }
    }
}

#[test]
fn timer_test_1() {
    check(|| {
        // Start 3 timers with delays 1,2,3 and values 10,20,40
        let timera = CountdownTimer::new(1, 10);
        let timerb = CountdownTimer::new(2, 20);
        let timerc = CountdownTimer::new(4, 40);
        let v1 = asynch::spawn(timera); // no need for async block
        let v2 = asynch::spawn(async move { timerb.await });
        let v3 = asynch::spawn(async move { timerc.await });
        // Spawn another task that waits for the timers and checks the return values
        asynch::spawn(async move {
            let sum = v1.await.unwrap() + v2.await.unwrap() + v3.await.unwrap();
            assert_eq!(sum, 10 + 20 + 40);
        });
    });
}
