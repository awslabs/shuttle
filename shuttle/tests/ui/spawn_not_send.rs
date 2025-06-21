use shuttle::future;
use shuttle::check_dfs;
use std::rc::Rc;

fn main() {
    check_dfs(
        || {
            let rc = Rc::new(0);
            shuttle::future::block_on(future::spawn(async { drop(rc) })).unwrap()
        },
        None,
    );
}