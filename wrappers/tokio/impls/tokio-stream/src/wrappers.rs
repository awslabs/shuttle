//! Wrappers for Tokio types that implement `Stream`.

// TODO: Implement `broadcast` and uncomment
/*
/// Error types for the wrappers.
pub mod errors {
    cfg_sync! {
        pub use crate::wrappers::broadcast::BroadcastStreamRecvError;
    }
}
*/

mod mpsc_bounded;
pub use mpsc_bounded::ReceiverStream;

mod mpsc_unbounded;
pub use mpsc_unbounded::UnboundedReceiverStream;

cfg_sync! {
    // TODO: Implement `broadcast` and uncomment
    /*
    mod broadcast;
    pub use broadcast::BroadcastStream;
    */

    mod watch;
    pub use watch::WatchStream;
}

// TODO: Implement `signal` and uncomment
/*
cfg_signal! {
    #[cfg(unix)]
    mod signal_unix;
    #[cfg(unix)]
    pub use signal_unix::SignalStream;

    #[cfg(any(windows, docsrs))]
    mod signal_windows;
    #[cfg(any(windows, docsrs))]
    pub use signal_windows::{CtrlCStream, CtrlBreakStream};
}
*/

cfg_time! {
    mod interval;
    pub use interval::IntervalStream;
}

// TODO: Implement `net` and uncomment
/*
cfg_net! {
    mod tcp_listener;
    pub use tcp_listener::TcpListenerStream;

    #[cfg(unix)]
    mod unix_listener;
    #[cfg(unix)]
    pub use unix_listener::UnixListenerStream;
}
*/

// TODO: Implement `io` and uncomment
/*
cfg_io_util! {
    mod split;
    pub use split::SplitStream;

    mod lines;
    pub use lines::LinesStream;
}
*/

// TODO: Implement `fs` and uncomment
/*
cfg_fs! {
    mod read_dir;
    pub use read_dir::ReadDirStream;
}
*/
