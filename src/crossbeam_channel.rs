//! Selector implementation for multi-producer, multi-consumer channels.

use crate::{sync::{Arc, mpsc}, runtime::{execution::ExecutionState, thread}};
use crossbeam_channel::{TrySelectError, SelectTimeoutError, RecvError, RecvTimeoutError, SendError, SendTimeoutError, TrySendError, TryRecvError};
use rand::Rng;
use core::fmt::Debug;
use std::time::Duration;
use crate::runtime::task::TaskId;

/// Create an unbounded channel
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    let channel = Arc::new(mpsc::Channel::new(None));
    let sender = Sender {
        inner: Arc::clone(&channel),
    };
    let receiver = Receiver {
        inner: Arc::clone(&channel),
    };
    (sender, receiver)
}

/// Represents the return value of a selector; contains an index representing which of the selectables was ready.
#[derive(Debug)]
pub struct SelectedOperation {
    /// the index representing which selectable became ready
    pub index: usize,
}

impl SelectedOperation {
    /// Returns the index of the selectable which became ready
    pub fn index(&self) -> usize {
        self.index
    }

    /// Performs a receive on an arbitrary receiver which had been given to the selector that returned this SelectedOperation.
    /// TODO: in crossbeam, this method panics if the receiver does not match the one added to the selector -- is this necessary?
    pub fn recv<T>(&self, r: &Receiver<T>) -> Result<T, RecvError> {
        r.recv().map_err(|_| RecvError)
    }
}

/// Any object which is selectable -- typically used for a receiver.
pub trait Selectable {
    /// Attempts to select from the selectable, returning true if anything is present and false otherwise.
    fn try_select(&self) -> bool;
    /// Adds a queued receiver to the selectable (used when the selector containing the selectable is about to block).
    fn add_waiting_receiver(&self, task: TaskId);
    /// Removes all instances of a queued receiver from the selectable (used after the selector has been unblocked).
    fn delete_waiting_receiver(&self, task: TaskId);
}

impl<'a> Debug for dyn Selectable + 'a {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Selectable")
    }
}

fn try_select(handles: &mut [(&dyn Selectable, usize)]) -> Result<SelectedOperation, TrySelectError> {
    for handle in handles {
        if handle.0.try_select() {
            return Ok(SelectedOperation{index: handle.1})
        }
    }
    Err(TrySelectError{})
}

fn select(handles: &mut [(&dyn Selectable, usize)]) -> SelectedOperation {
    SelectedOperation {
        index: {
            if let Ok(SelectedOperation{index: idx}) = try_select(handles) {
                idx
            } else {
                let id = ExecutionState::me();

                for handle in &mut *handles {
                    handle.0.add_waiting_receiver(id);
                }
                
                loop {
                    ExecutionState::with(|state| {
                        state.get_mut(id).block()
                    });
                    thread::switch();

                    if let Ok(SelectedOperation{index: idx}) = try_select(handles) {
                        for handle in &mut *handles {
                            handle.0.delete_waiting_receiver(id);
                        }
                        break idx;
                    }
                }
            }
        },
    }
}

/// A selector.
#[derive(Debug)]
pub struct Select<'a> {
    handles: Vec<(&'a dyn Selectable, usize)>,
}

impl<'a> Select<'a> {
    /// Creates a new instance of the selector with no selectables.
    pub fn new() -> Self {
        Self { handles: Vec::new() }
    }

    /// Adds a new receiving selectable which the selector will wait on.
    pub fn recv<T>(&mut self, r: &'a Receiver<T>) -> usize {
        self.handles.push((r, self.handles.len()));
        self.handles.len() - 1
    }

    /// Attempts to receive from one of the added selectables, returning the index of the given channel if possible.
    pub fn try_select(&mut self) -> Result<SelectedOperation, TrySelectError> {
        try_select(&mut self.handles)
    }

    /// Blocks until a value can be retrieved from one of the given selectables.
    pub fn select(&mut self) -> SelectedOperation {
        select(&mut self.handles)
    }

    /// Blocks until a value can be retrieved from one of the given selectables, returning an error if no value is received
    /// before the timeout.
    pub fn select_timeout(&mut self, d: Duration) -> Result<SelectedOperation, SelectTimeoutError>  {
        match self.try_select() {
            Ok(v) => Ok(v),
            Err(TrySelectError) => {
                let mut rng = rand::thread_rng();
                if rng.gen::<f64>() < timeout_duration_to_success_probability(d) {
                    Ok(self.select())
                } else {
                Err(SelectTimeoutError)
                }
            }
        }
    }
}

#[derive(Debug)]
/// Represents the consumer portion of a Crossbeam multi-producer, multi-consumer channel.
pub struct Receiver<T> { 
    inner: Arc<mpsc::Channel<T>>,
}

impl<T> Receiver<T> {
    /// Tries to receive; returns an error if the receive cannot take place.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.inner.try_recv() {
            Ok(res) => {
                Ok(res)
            },
            Err(std::sync::mpsc::TryRecvError::Empty) => Err(TryRecvError::Empty),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(TryRecvError::Disconnected),
        }
    }
    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up.
    pub fn recv(&self) -> Result<T, RecvError> {
        self.inner.recv().map_err(|_| RecvError)
    }

    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up, or if it waits more than timeout.
    pub fn recv_timeout(&self, d: Duration) -> Result<T, RecvTimeoutError> {
        match self.inner.try_recv() {
            Ok(res) => {
                Ok(res)
            },
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let mut rng = rand::thread_rng();
                if rng.gen::<f64>() < timeout_duration_to_success_probability(d) {
                    self.inner.recv().map_err(|_| RecvTimeoutError::Disconnected)
                } else {
                    Err(RecvTimeoutError::Timeout)
                }
            },
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(RecvTimeoutError::Disconnected)
            },
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.inner.inc_receiver_count();
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Selectable for Receiver<T> {
    // Determines whether the channel has anything to be received.
    // TODO (Finn) is this sufficient?
    fn try_select(&self) -> bool {
        self.inner.state.borrow().has_messages()
    }

    fn add_waiting_receiver(&self, task: TaskId) {
        self.inner.state.borrow_mut().add_waiting_receiver(task)
    }

    fn delete_waiting_receiver(&self, task: TaskId) {
        self.inner.state.borrow_mut().delete_waiting_receiver(task)
    }
}

// Returns a probability that a message will succeed based on the given timeout duration.
fn timeout_duration_to_success_probability(_: Duration) -> f64 {
    0.8 // TODO: mathematical expression such that success probability increases with timeout
}

#[derive(Debug)]
/// Represents the producer portion of a Crossbeam multi-producer, multi-consumer channel.
pub struct Sender<T> {
    inner: Arc<mpsc::Channel<T>>,
}

impl<T> Sender<T> {
    /// Tries to send; returns an error if the send cannot take place.
    pub fn try_send(&self, t: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(t).map_err(|e| match e {
            std::sync::mpsc::TrySendError::Full(m) => TrySendError::Full(m),
            std::sync::mpsc::TrySendError::Disconnected(m) => TrySendError::Disconnected(m)
        }) // converting the error from std::sync::mpsc to crossbeam_channel
    }

    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t).map_err(|e| SendError(e.0)) // converting sync::mpsc::SendError to crossbeam_channel::SendError
    }

    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    pub fn send_timeout(&self, t: T, d: Duration) -> Result<(), SendTimeoutError<T>> {
        match self.try_send(t) {
            Ok(m) => Ok(m),
            Err(TrySendError::Full(message)) => {
                let mut rng = rand::thread_rng();
                if rng.gen::<f64>() < timeout_duration_to_success_probability(d) {
                    self.send(message).map_err(|e| SendTimeoutError::Timeout(e.0))
                } else {
                    Err(SendTimeoutError::Timeout(message))
                }
            },
            Err(TrySendError::Disconnected(message)) => {
                Err(SendTimeoutError::Disconnected(message))
            }
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.inner.inc_sender_count();
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.inner.drop_sender()
    }
}
