use shuttle::sync::mpsc::{channel};
use shuttle::sync::selector::Select;
use shuttle::{check_dfs};
use test_log::test;

#[test]
fn selector_one_channel() {
    check_dfs(
        move || {
            let (s, r) = channel();

            let mut selector = Select::new();
            selector.recv(&r);

            s.send(5).unwrap();

            let idx = selector.select();
            assert_eq!(idx, 0);

            let val = r.recv().unwrap();
            assert_eq!(val, 5);
        },
        None,
    );
}

#[test]
fn selector_multi_channel() {
    check_dfs(
        move || {
            let (_, r1) = channel::<i32>();
            let (s2, r2) = channel();

            let mut selector = Select::new();
            selector.recv(&r1);
            selector.recv(&r2);

            s2.send(81).unwrap();

            let idx = selector.select();
            assert_eq!(idx, 1);

            let val = r2.recv().unwrap();
            assert_eq!(val, 81);
        },
        None,
    );
}

#[test]
fn try_select_empty_selector() {
    check_dfs(
        move || {
            assert_eq!(Select::new().try_select(), None)
        },
        None,
    );
}
