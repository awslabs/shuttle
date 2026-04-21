/// Specifies under which conditions a retry is attempted.
pub trait Condition<E> {
    fn should_retry(&mut self, error: &E) -> bool;
}

impl<E, F: FnMut(&E) -> bool> Condition<E> for F {
    fn should_retry(&mut self, error: &E) -> bool {
        self(error)
    }
}
