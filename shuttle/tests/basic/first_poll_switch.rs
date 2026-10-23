//! Tests for `FirstPollSwitch`, which puts a scheduling point at the first poll of a future.

use shuttle::future::block_on;
use shuttle::{check_dfs, current};
use shuttle_engine::runtime::thread::FirstPollSwitch;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A future that is ready on its `polls`th poll, and records which of its polls switched.
struct PollsUntilReady {
    first_poll: FirstPollSwitch,
    polls: usize,
    switched: Vec<bool>,
}

impl Future for PollsUntilReady {
    type Output = Vec<bool>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<bool>> {
        let before = current::context_switches();
        let first = self.first_poll.switch();
        let switched = current::context_switches() > before;
        assert_eq!(first, switched);
        self.switched.push(switched);
        if self.switched.len() == self.polls {
            Poll::Ready(std::mem::take(&mut self.switched))
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[test]
fn first_poll_switch_switches_once() {
    check_dfs(
        || {
            let switched = block_on(PollsUntilReady {
                first_poll: FirstPollSwitch::new(),
                polls: 3,
                switched: Vec::new(),
            });
            assert_eq!(switched, [true, false, false]);
        },
        None,
    );
}
