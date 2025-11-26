// There is nothing in tokio-util::codec which relies on tokio (apart from the traits AsyncRead/AsyncWrite) or std::sync.
// It should therefore be fine to simply reexport codec from tokio-util.
// Note that this is not code which is really used, or is going to be used much, and the reason we have it here
// is mostly for convenience, ie., it should "just work" to swap out tokio-util with shuttle-tokio-util.

pub use tokio_util_orig::codec;