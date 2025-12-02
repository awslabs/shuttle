#![warn(rust_2018_idioms)]
#![cfg(all(feature = "time", feature = "sync", feature = "io-util"))]

use shuttle_tokio_stream_impl::StreamExt;
use tokio::time;
use tokio_test::*;

use std::time::Duration;

// TODO: This test is `ignore`d because it relies on pausing and manually advancing time,
//       and thus fail as time is currently unsupported. However, it is good tests to have if
//       we ever add time support, which is why it is not removed entirely.

#[ignore]
#[tokio::test]
async fn usage() {
    time::pause();

    let mut stream = task::spawn(futures::stream::repeat(()).throttle(Duration::from_millis(100)));

    assert_ready!(stream.poll_next());
    assert_pending!(stream.poll_next());

    time::advance(Duration::from_millis(90)).await;

    assert_pending!(stream.poll_next());

    time::advance(Duration::from_millis(101)).await;

    assert!(stream.is_woken());

    assert_ready!(stream.poll_next());
}
