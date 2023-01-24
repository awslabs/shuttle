//! Selector implementation for multi-producer, single-consumer channels.

use crate::{sync::mpsc::Receiver, runtime::{execution::ExecutionState, thread}};
use core::fmt::Debug;
use crate::runtime::task::TaskId;

/// Any object which is selectable -- typically used for a receiver.
pub trait Selectable {
    /// Attempts to select from the selectable, returning true if anything is present and false otherwise.
    fn try_select(&self) -> bool;
    /// Adds a queued receiver to the selectable (used when the receiver is about to block).
    fn add_waiting_receiver(&self, task: TaskId);
    /// Removes all instances of a queued receiver from the selectable (used after the receiver has been unblocked).
    fn delete_waiting_receiver(&self, task: TaskId);
}

impl<'a> Debug for dyn Selectable + 'a {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Selectable")
    }
}

fn try_select(handles: &mut [(&dyn Selectable, usize)]) -> Option<usize> {
    for handle in handles {
        if handle.0.try_select() {
            return Some(handle.1)
        }
    }
    None
}

fn select(handles: &mut [(&dyn Selectable, usize)]) -> usize {
    if let Some(idx) = try_select(handles) {
        return idx
    }

    let id = ExecutionState::me();

    loop {
        for handle in &mut *handles {
            handle.0.add_waiting_receiver(id);
        }

        ExecutionState::with(|state| {
            state.get_mut(id).block()
        });
        thread::switch();

        if let Some(idx) = try_select(handles) {
            for handle in &mut *handles {
                handle.0.delete_waiting_receiver(id);
            }
            break idx;
        }
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
    pub fn recv<T>(&mut self, r: &'a Receiver<T>) {
        self.handles.push((r, self.handles.len()))
    }

    /// Attempts to receive from one of the added selectables, returning the index of the given channel if possible.
    pub fn try_select(&mut self) -> Option<usize> {
        try_select(&mut self.handles)
    }

    /// Blocks until a value can be retrieved from one of the given selectables.
    pub fn select(&mut self) -> usize {
        select(&mut self.handles)
    }
}