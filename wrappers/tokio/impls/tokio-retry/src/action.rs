use std::future::Future;

/// An action can be run multiple times and produces a future.
pub trait Action {
    /// The future that this action produces.
    type Future: Future<Output = Result<Self::Item, Self::Error>>;
    /// The item that the future may resolve with.
    type Item;
    /// The error that the future may resolve with.
    type Error;

    fn run(&mut self) -> Self::Future;
}

impl<R, E, T: Future<Output = Result<R, E>>, F: FnMut() -> T> Action for F {
    type Item = R;
    type Error = E;
    type Future = T;

    fn run(&mut self) -> Self::Future {
        self()
    }
}
