//! The `std::sync::mpsc` channels, invisible to Shuttle.
//!
//! Each type is an alias of the `std` type, and each function calls the `std` function.

/// [`std::sync::mpsc::Sender`], invisible to Shuttle.
pub type Sender<T> = std::sync::mpsc::Sender<T>;

/// [`std::sync::mpsc::SyncSender`], invisible to Shuttle.
pub type SyncSender<T> = std::sync::mpsc::SyncSender<T>;

/// [`std::sync::mpsc::Receiver`], invisible to Shuttle.
pub type Receiver<T> = std::sync::mpsc::Receiver<T>;

// The errors of the channels above. The lint gate doesn't deny them, so plain re-exports pass
// it.
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError};

/// [`std::sync::mpsc::channel`], invisible to Shuttle.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    std::sync::mpsc::channel()
}

/// [`std::sync::mpsc::sync_channel`], invisible to Shuttle.
pub fn sync_channel<T>(bound: usize) -> (SyncSender<T>, Receiver<T>) {
    std::sync::mpsc::sync_channel(bound)
}
