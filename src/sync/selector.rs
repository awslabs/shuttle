//! Selector implementation for multi-producer, single-consumer channels.

use crate::{sync::mpsc::Receiver, runtime::execution::ExecutionState};
use core::fmt::Debug;
use crate::runtime::task::TaskId;

pub trait Selectable {
    fn try_select(&self) -> bool;
    fn add_waiting_receiver(&mut self, task: TaskId);
    fn delete_waiting_receiver(&mut self, task: TaskId);
}

impl Debug for dyn Selectable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Series{{}}")
    }
}

fn try_select(handles: &mut [(&mut dyn Selectable, usize, *const u8)]) -> Option<(usize, *const u8)> {
    for handle in handles {
        if handle.0.try_select() {
            return Some((handle.1, handle.2))
        }
    }
    None
}

fn select(handles: &mut [(&mut dyn Selectable, usize, *const u8)]) -> usize {
    if let Some((idx, _)) = try_select(handles) {
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

        if let Some((idx, _)) = try_select(handles) {
            for handle in &mut *handles {
                handle.0.delete_waiting_receiver(id);
            }
            return idx
        }
    }
}

// A selector.
// #[derive(Debug)]
pub struct Select<'a> {
    handles: Vec<(&'a mut dyn Selectable, usize, *const u8)>,
}

impl<'a> Select<'a> {
    pub fn new() -> Self {
        Self { handles: Vec::new() }
    }

    pub fn recv<T>(&mut self, r: &'a mut Receiver<T>) {
        let ptr = r as *const Receiver<_> as *const u8;
        self.handles.push((r, self.handles.len(), ptr))
    }

    pub fn try_select(&mut self) -> Option<usize> {
        try_select(&mut self.handles).map(|(idx, _)| idx)
    }

    pub fn select(&mut self) -> usize {
        select(&mut self.handles)
    }
}