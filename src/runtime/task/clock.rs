use crate::runtime::task::{TaskId, DEFAULT_INLINE_TASKS};
use smallvec::{smallvec, SmallVec};
use std::cmp::{Ordering, PartialOrd};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorClock {
    pub(crate) time: SmallVec<[u32; DEFAULT_INLINE_TASKS]>,
}

impl VectorClock {
    pub(crate) const fn new() -> Self {
        Self {
            time: SmallVec::new_const(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_from(v: &[u32]) -> Self {
        Self {
            time: SmallVec::from(v),
        }
    }

    // Zero extend clock to accommodate `task_id` tasks.
    pub(crate) fn extend(&mut self, task_id: TaskId) {
        let num_new_tasks = 1 + task_id.0 - self.time.len();
        let clock: SmallVec<[_; DEFAULT_INLINE_TASKS]> = smallvec![0u32; num_new_tasks];
        self.time.extend_from_slice(&clock);
    }

    pub(crate) fn increment(&mut self, task_id: TaskId) {
        self.time[task_id.0] += 1;
    }

    // Update the clock of `self` with the clock from `other`
    pub(crate) fn update(&mut self, other: &Self) {
        let n1 = self.time.len();
        let n2 = other.time.len();
        for i in 0..n1.min(n2) {
            self.time[i] = self.time[i].max(other.time[i])
        }
        for i in n1..n2 {
            // could be empty
            self.time.push(other.time[i]);
        }
    }

    pub fn get(&self, i: usize) -> u32 {
        self.time[i]
    }
}

impl std::ops::Deref for VectorClock {
    type Target = [u32];
    fn deref(&self) -> &Self::Target {
        &self.time[..]
    }
}

fn unify(a: Ordering, b: Ordering) -> Option<Ordering> {
    use Ordering::*;

    match (a, b) {
        (Equal, Equal) => Some(Equal),
        (Less, Greater) | (Greater, Less) => None,
        (Less, _) | (_, Less) => Some(Less),
        (Greater, _) | (_, Greater) => Some(Greater),
    }
}

impl PartialOrd for VectorClock {
    // Compare vector clocks
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let n1 = self.time.len();
        let n2 = other.time.len();
        // if (n1<n2), then other can't have happened before self, similarly for (n1>n2)
        let mut ord = n1.cmp(&n2);
        for i in 0..n1.min(n2) {
            ord = unify(ord, self.time[i].cmp(&other.time[i]))?; // return if incomparable
        }
        Some(ord)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn vector_clock() {
        let v1 = VectorClock::new_from(&[1, 2, 3, 4]);
        let v2 = VectorClock::new_from(&[1, 2, 4, 5]);
        let v3 = VectorClock::new_from(&[1, 2, 3, 1]);
        let v4 = VectorClock::new_from(&[1, 2, 4, 1]);
        let v5 = VectorClock::new_from(&[1, 2, 3, 4]);
        assert!(v1 < v2 && v1 > v3 && v1 == v5);
        assert!(v2 > v3 && v2 > v4);
        assert!(v3 < v4);
        assert_eq!(v1.partial_cmp(&v4), None);

        let v1 = VectorClock::new_from(&[1, 2, 3, 4]);
        let v2 = VectorClock::new_from(&[1, 2, 2]);
        let v3 = VectorClock::new_from(&[1, 2, 3]);
        let v4 = VectorClock::new_from(&[1, 2, 4]);
        assert!(v1 > v2);
        assert!(v1 > v3);
        assert_eq!(v1.partial_cmp(&v4), None);

        let v1 = VectorClock::new_from(&[]);
        let v2 = VectorClock::new_from(&[1]);
        assert!(v1 < v2);

        let v1 = VectorClock::new_from(&[1, 2, 1]);
        let v2 = VectorClock::new_from(&[1, 3]);
        let v3 = VectorClock::new_from(&[1, 1, 1, 2]);
        let v4 = VectorClock::new_from(&[1, 1, 2]);

        let mut v = v1.clone();
        v.update(&v2);
        assert_eq!(v, VectorClock::new_from(&[1, 3, 1]));

        let mut v = v1.clone();
        v.update(&v3);
        assert_eq!(v, VectorClock::new_from(&[1, 2, 1, 2]));

        let mut v = v1.clone();
        v.update(&v4);
        assert_eq!(v, VectorClock::new_from(&[1, 2, 2]));

        let mut v = v1.clone();
        v.update(&VectorClock::new());
        assert_eq!(v, v1);
    }
}
