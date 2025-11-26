//! A multi-producer, multi-consumer broadcast queue. Each sent value is seen by
//! all consumers.
//!
//! Currently a stub implementation, we need this around for cargo check but we
//! don't need this to actually run.

use std::marker::PhantomData;

pub mod error {
    pub use tokio::sync::broadcast::error::*;
}

pub fn channel<T: Clone>(_capacity: usize) -> (Sender<T>, Receiver<T>) {
    let tx = Sender(PhantomData);
    let rx = Receiver(PhantomData);
    (tx, rx)
}

#[derive(Debug)]
pub struct Sender<T>(PhantomData<T>);

#[derive(Debug)]
pub struct Receiver<T>(PhantomData<T>);

impl<T> Receiver<T> {
    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        todo!()
    }
}

impl<T: Clone> Receiver<T> {
    pub fn resubscribe(&self) -> Self {
        todo!()
    }

    pub async fn recv(&mut self) -> Result<T, error::RecvError> {
        todo!()
    }

    pub async fn try_recv(&mut self) -> Result<T, error::TryRecvError> {
        todo!()
    }
}

impl<T> Sender<T> {
    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        todo!()
    }

    pub fn send(&self, _value: T) -> Result<usize, error::SendError<T>> {
        todo!()
    }

    pub fn subscribe(&self) -> Receiver<T> {
        todo!()
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}
