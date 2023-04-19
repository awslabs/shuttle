use futures::future::join_all;
use shuttle::{
    check_random,
    current::{get_tag_for_current_task, set_tag_for_current_task, Tag},
    future::block_on,
    thread,
    thread::JoinHandle,
};
use test_log::test;

fn spawn_some_futures_and_set_tag<F: (Fn(Tag, u64) -> Tag) + Send + Sync>(
    tag_on_entry: Tag,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            shuttle::future::spawn(async move {
                assert_eq!(get_tag_for_current_task(), tag_on_entry);
                let new_tag = f(tag_on_entry, i);
                set_tag_for_current_task(new_tag);
                assert_eq!(get_tag_for_current_task(), new_tag);
            })
        })
        .collect();

    block_on(join_all(threads));

    assert_eq!(get_tag_for_current_task(), tag_on_entry);
}

fn spawn_some_threads_and_set_tag<F: (Fn(Tag, u64) -> Tag) + Send + Sync>(
    tag_on_entry: Tag,
    f: &'static F,
    num_threads: u64,
) {
    let threads: Vec<_> = (0..num_threads)
        .map(|i| {
            thread::spawn(move || {
                assert_eq!(get_tag_for_current_task(), tag_on_entry);
                let new_tag = f(tag_on_entry, i);
                set_tag_for_current_task(new_tag);
                assert_eq!(get_tag_for_current_task(), new_tag);
            })
        })
        .collect();

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert_eq!(get_tag_for_current_task(), tag_on_entry);
}

fn spawn_threads_which_spawn_more_threads(
    tag_on_entry: Tag,
    num_threads_first_block: u64,
    num_threads_second_block: u64,
) {
    let mut threads: Vec<_> = (0..num_threads_first_block)
        .map(|i| {
            thread::spawn(move || {
                assert_eq!(get_tag_for_current_task(), tag_on_entry);
                let new_tag = i.into();
                set_tag_for_current_task(new_tag);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|_, _| 123.into(), 13);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|_, x| (x * 13).into(), 7);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_threads_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) + x).into(), 19);
                assert_eq!(get_tag_for_current_task(), new_tag);
                spawn_some_futures_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) & x).into(), 17);
                assert_eq!(get_tag_for_current_task(), new_tag);
            })
        })
        .collect();

    assert_eq!(get_tag_for_current_task(), tag_on_entry);

    let new_tag_main_thread = 987654321.into();
    set_tag_for_current_task(new_tag_main_thread);
    assert_eq!(get_tag_for_current_task(), new_tag_main_thread);

    threads.extend(
        (0..num_threads_second_block)
            .map(|i| {
                thread::spawn(move || {
                    assert_eq!(get_tag_for_current_task(), new_tag_main_thread);
                    let new_tag = i.into();
                    set_tag_for_current_task(new_tag);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|_, _| 123.into(), 13);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|_, x| (x * 13).into(), 7);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_threads_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) + x).into(), 19);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                    spawn_some_futures_and_set_tag(new_tag, &|p, x| ((u64::from(p) << 4) & x).into(), 17);
                    assert_eq!(get_tag_for_current_task(), new_tag);
                })
            })
            .collect::<Vec<_>>(),
    );

    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    assert_eq!(get_tag_for_current_task(), new_tag_main_thread);
}

#[test]
fn threads_which_spawn_threads_which_spawn_threads() {
    check_random(|| spawn_threads_which_spawn_more_threads(Tag::default(), 15, 10), 10)
}

fn spawn_thread_and_set_tag(tag_on_entry: Tag, new_tag: Tag) -> JoinHandle<u64> {
    thread::spawn(move || {
        assert_eq!(get_tag_for_current_task(), tag_on_entry);
        set_tag_for_current_task(new_tag);
        assert_eq!(get_tag_for_current_task(), new_tag);
        new_tag.into()
    })
}

fn spawn_and_join() {
    set_tag_for_current_task(42.into());
    let h1 = spawn_thread_and_set_tag(42.into(), 84.into());
    set_tag_for_current_task(50.into());
    let h2 = spawn_thread_and_set_tag(50.into(), 100.into());
    let results = [h1.join().unwrap(), h2.join().unwrap()];
    assert_eq!(results, [84, 100]);
}

#[test]
fn test_spawn_and_join() {
    check_random(spawn_and_join, 20)
}
