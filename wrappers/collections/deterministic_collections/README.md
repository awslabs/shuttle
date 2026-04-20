# Deterministic collections

This crate provides `HashMap` and `HashSet` implementations with deterministic iteration order. Most users will not want to depend on this directly, and depend on `determinizable_collections`, which allows toggling between deterministic collections and the collections from std, instead.

## How it works

The standard library's `HashMap` and `HashSet` use a randomly-seeded hasher (`RandomState`) which produces different iteration orders across runs. This crate replaces the random seed with a fixed one, making iteration order deterministic and reproducible.

The types are newtypes around the standard library collections, forwarding most operations via `Deref`/`DerefMut` and re-implementing constructors and trait impls that interact with the hasher state.

## Limitations

Using deterministic collections loses the security benefits of randomly-seeded hashing (HashDoS resilience) and the "incidental" benefits of non-deterministic ordering, such as ordering-specific bugs surfacing intermittently or multiple instances not accessing resources in the same order. Consider these trade-offs before enabling in production.
