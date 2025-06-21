use std::any::Any;
use std::collections::{HashMap, VecDeque};

/// A unique identifier for a storage slot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StorageKey(pub usize, pub usize); // (identifier, type)

/// A map of storage values.
///
/// We remember the insertion order into the storage HashMap so that destruction is deterministic.
/// Values are Option<_> because we need to be able to incrementally destruct them, as it's valid
/// for TLS destructors to initialize new TLS slots. When a slot is destructed, its key is removed
/// from `order` and its value is replaced with None.
#[derive(Debug)]
pub(crate) struct StorageMap {
    locals: HashMap<StorageKey, Option<Box<dyn Any>>>,
    order: VecDeque<StorageKey>,
}

impl StorageMap {
    pub fn new() -> Self {
        Self {
            locals: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get<T: 'static>(&self, key: StorageKey) -> Option<Result<&T, AlreadyDestructedError>> {
        self.locals.get(&key).map(|val| {
            val.as_ref()
                .map(|val| {
                    Ok(val
                        .downcast_ref::<T>()
                        .expect("local value must downcast to expected type"))
                })
                .unwrap_or(Err(AlreadyDestructedError))
        })
    }

    pub fn init<T: 'static>(&mut self, key: StorageKey, value: T) {
        let result = self.locals.insert(key, Some(Box::new(value)));
        assert!(result.is_none(), "cannot reinitialize a storage slot");
        self.order.push_back(key);
    }

    /// Return ownership of the next still-initialized storage slot.
    pub fn pop(&mut self) -> Option<Box<dyn Any>> {
        let key = self.order.pop_front()?;
        let value = self
            .locals
            .get_mut(&key)
            .expect("keys in `order` must exist")
            .take()
            .expect("keys in `order` must not yet be destructed");
        Some(value)
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct AlreadyDestructedError;
