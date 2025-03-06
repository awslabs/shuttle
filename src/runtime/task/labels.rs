//! Labels are a way to attach an arbitrary set of values with a task, with the only
//! constraint being that at most one value of a given type `T` can be attached at any time,
//! using `get::<T>()` to retrieve, and `set::<T>(..)` to set the value.
//! Labels behave almost identically to [Extensions](https://docs.rs/http/latest/http/struct.Extensions.html)
//! in the `http` crate.

/*
** The code below is directly copied from https://github.com/hyperium/http/blob/master/src/extensions.rs
** but renaming 'Extensions' to 'Labels'
**
** The key idea is to keep a HashMap (named `AnyMap`) that maps the `TypeId` for a type
** to its associated value, so a `get::<T>()` is translated to `get(TypeId::of::<T>())`.
*/

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};

type AnyMap = HashMap<TypeId, Box<dyn AnyClone>, BuildHasherDefault<IdHasher>>;

// With TypeIds as keys, there's no need to hash them. They are already hashes
// themselves, coming from the compiler. The IdHasher just holds the u64 of
// the TypeId, and then returns it, instead of doing any bit fiddling.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn write(&mut self, _: &[u8]) {
        unreachable!("TypeId calls write_u64");
    }

    #[inline]
    fn write_u64(&mut self, id: u64) {
        self.0 = id;
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// A collections of assigned Labels
///
/// `Labels` can be used to store extra data associated with running tasks.
#[derive(Clone, Default)]
pub struct Labels {
    // If Labels are never used, no need to carry around an empty HashMap.
    // That's 3 words. Instead, this is only 1 word.
    map: Option<Box<AnyMap>>,
}

impl Labels {
    /// Create an empty `Labels`.
    #[inline]
    pub fn new() -> Labels {
        Labels { map: None }
    }

    /// Insert a type into this `Labels`.
    ///
    /// If a label of this type already existed, it will
    /// be returned.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// assert!(ext.insert(5i32).is_none());
    /// assert!(ext.insert(4u8).is_none());
    /// assert_eq!(ext.insert(9i32), Some(5i32));
    /// ```
    pub fn insert<T: Clone + 'static>(&mut self, val: T) -> Option<T> {
        self.map
            .get_or_insert_with(Box::default)
            .insert(TypeId::of::<T>(), Box::new(val))
            .and_then(|boxed| boxed.into_any().downcast().ok().map(|boxed| *boxed))
    }

    /// Get a reference to a type previously inserted on this `Labels`.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// assert!(ext.get::<i32>().is_none());
    /// ext.insert(5i32);
    ///
    /// assert_eq!(ext.get::<i32>(), Some(&5i32));
    /// ```
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.map
            .as_ref()
            .and_then(|map| map.get(&TypeId::of::<T>()))
            .and_then(|boxed| (**boxed).as_any().downcast_ref())
    }

    /// Get a mutable reference to a type previously inserted on this `Labels`.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// ext.insert(String::from("Hello"));
    /// ext.get_mut::<String>().unwrap().push_str(" World");
    ///
    /// assert_eq!(ext.get::<String>().unwrap(), "Hello World");
    /// ```
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.map
            .as_mut()
            .and_then(|map| map.get_mut(&TypeId::of::<T>()))
            .and_then(|boxed| (**boxed).as_any_mut().downcast_mut())
    }

    /// Get a mutable reference to a type, inserting `value` if not already present on this
    /// `Labels`.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// *ext.get_or_insert(1i32) += 2;
    ///
    /// assert_eq!(*ext.get::<i32>().unwrap(), 3);
    /// ```
    pub fn get_or_insert<T: Clone + 'static>(&mut self, value: T) -> &mut T {
        self.get_or_insert_with(|| value)
    }

    /// Get a mutable reference to a type, inserting the value created by `f` if not already present
    /// on this `Labels`.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// *ext.get_or_insert_with(|| 1i32) += 2;
    ///
    /// assert_eq!(*ext.get::<i32>().unwrap(), 3);
    /// ```
    pub fn get_or_insert_with<T: Clone + 'static, F: FnOnce() -> T>(&mut self, f: F) -> &mut T {
        let out = self
            .map
            .get_or_insert_with(Box::default)
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(f()));
        (**out).as_any_mut().downcast_mut().unwrap()
    }

    /// Get a mutable reference to a type, inserting the type's default value if not already present
    /// on this `Labels`.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// *ext.get_or_insert_default::<i32>() += 2;
    ///
    /// assert_eq!(*ext.get::<i32>().unwrap(), 2);
    /// ```
    pub fn get_or_insert_default<T: Default + Clone + 'static>(&mut self) -> &mut T {
        self.get_or_insert_with(T::default)
    }

    /// Remove a type from this `Labels`.
    ///
    /// If a label of this type existed, it will be returned.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// ext.insert(5i32);
    /// assert_eq!(ext.remove::<i32>(), Some(5i32));
    /// assert!(ext.get::<i32>().is_none());
    /// ```
    pub fn remove<T: 'static>(&mut self) -> Option<T> {
        self.map
            .as_mut()
            .and_then(|map| map.remove(&TypeId::of::<T>()))
            .and_then(|boxed| boxed.into_any().downcast().ok().map(|boxed| *boxed))
    }

    /// Clear all inserted labels
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// ext.insert(5i32);
    /// ext.clear();
    ///
    /// assert!(ext.get::<i32>().is_none());
    /// ```
    #[inline]
    pub fn clear(&mut self) {
        if let Some(ref mut map) = self.map {
            map.clear();
        }
    }

    /// Check whether the label set is empty or not.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// assert!(ext.is_empty());
    /// ext.insert(5i32);
    /// assert!(!ext.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.as_ref().is_none_or(|map| map.is_empty())
    }

    /// Get the number of Labels available.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext = Labels::new();
    /// assert_eq!(ext.len(), 0);
    /// ext.insert(5i32);
    /// assert_eq!(ext.len(), 1);
    /// ```
    #[inline]
    pub fn len(&self) -> usize {
        self.map.as_ref().map_or(0, |map| map.len())
    }

    /// Extends `self` with another `Labels`.
    ///
    /// If an instance of a specific type exists in both, the one in `self` is overwritten with the
    /// one from `other`.
    ///
    /// # Example
    ///
    /// ```
    /// # use shuttle::current::Labels;
    /// let mut ext_a = Labels::new();
    /// ext_a.insert(8u8);
    /// ext_a.insert(16u16);
    ///
    /// let mut ext_b = Labels::new();
    /// ext_b.insert(4u8);
    /// ext_b.insert("hello");
    ///
    /// ext_a.extend(ext_b);
    /// assert_eq!(ext_a.len(), 3);
    /// assert_eq!(ext_a.get::<u8>(), Some(&4u8));
    /// assert_eq!(ext_a.get::<u16>(), Some(&16u16));
    /// assert_eq!(ext_a.get::<&'static str>().copied(), Some("hello"));
    /// ```
    pub fn extend(&mut self, other: Self) {
        if let Some(other) = other.map {
            if let Some(map) = &mut self.map {
                map.extend(*other);
            } else {
                self.map = Some(other);
            }
        }
    }
}

impl fmt::Debug for Labels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Labels").finish()
    }
}

trait AnyClone: Any {
    fn clone_box(&self) -> Box<dyn AnyClone>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Clone + 'static> AnyClone for T {
    fn clone_box(&self) -> Box<dyn AnyClone> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl Clone for Box<dyn AnyClone> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

#[test]
fn test_labels() {
    #[derive(Clone, Debug, PartialEq)]
    struct MyType(i32);

    let mut labels = Labels::new();

    labels.insert(5i32);
    labels.insert(MyType(10));

    assert_eq!(labels.get(), Some(&5i32));
    assert_eq!(labels.get_mut(), Some(&mut 5i32));

    let ext2 = labels.clone();

    assert_eq!(labels.remove::<i32>(), Some(5i32));
    assert!(labels.get::<i32>().is_none());

    // clone still has it
    assert_eq!(ext2.get(), Some(&5i32));
    assert_eq!(ext2.get(), Some(&MyType(10)));

    assert_eq!(labels.get::<bool>(), None);
    assert_eq!(labels.get(), Some(&MyType(10)));
}
