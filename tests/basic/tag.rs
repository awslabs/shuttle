use futures::future::join_all;
use shuttle::{check_random, future::block_on, get_tag, set_tag, thread};
use test_log::test;

fn spawn_some_futures_and_set_tag<F: (Fn(u64, u64) -> u64) + Send + Sync>(
    tag_on_entry: u64,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            shuttle::future::spawn(async move {
                assert!(get_tag() == tag_on_entry);
                set_tag(f(tag_on_entry, i));
                assert!(get_tag() == f(tag_on_entry, i));
            })
        })
        .collect();

    block_on(join_all(threads));
}

fn spawn_some_threads_and_set_tag<F: (Fn(u64, u64) -> u64) + Send + Sync>(
    tag_on_entry: u64,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            thread::spawn(move || {
                assert!(get_tag() == tag_on_entry);
                set_tag(f(tag_on_entry, i));
                assert!(get_tag() == f(tag_on_entry, i));
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert!(get_tag() == tag_on_entry);
}

fn spawn_threads_which_spawn_more_threads(tag_on_entry: u64, num_threads: u64) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            thread::spawn(move || {
                assert!(get_tag() == tag_on_entry);
                set_tag(i);
                assert!(get_tag() == i);
                spawn_some_threads_and_set_tag(i, &|_, _| 123, 13);
                assert!(get_tag() == i);
                spawn_some_threads_and_set_tag(i, &|_, x| x * 13, 7);
                assert!(get_tag() == i);
                spawn_some_threads_and_set_tag(i, &|p, x| (p << 4) + x, 19);
                assert!(get_tag() == i);
                spawn_some_futures_and_set_tag(i, &|p, x| (p << 4) & x, 17);
                assert!(get_tag() == i);
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert!(get_tag() == tag_on_entry);
}

#[test]
fn threads_which_spawn_threads_which_spawn_threads() {
    check_random(|| spawn_threads_which_spawn_more_threads(0, 15), 10)
}
