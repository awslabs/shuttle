/// Parse [`Uuid`](crate::Uuid)s from string literals at compile time: uuid's `uuid!`, for this
/// crate's `Uuid`.
#[macro_export]
macro_rules! uuid {
    ($uuid:expr) => {{
        const OUTPUT: $crate::Uuid = match $crate::Uuid::try_parse($uuid) {
            $crate::__macro_support::Ok(u) => u,
            $crate::__macro_support::Err(_) => panic!("invalid UUID"),
        };
        OUTPUT
    }};
}
