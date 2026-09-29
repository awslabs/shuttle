//! Frame-pointer backtraces for Apple arm64.
//!
//! Apple's arm64 ABI requires `x29` to always point at a valid *frame record*: two words holding the
//! caller's `x29` and the return address. Every function that makes a call pushes one, so the
//! records form a linked list from the innermost frame outwards, and a backtrace is two loads per
//! frame. Symbolizing the return addresses is the expensive part, and waits until the backtrace is
//! printed.
//!
//! What a frame-pointer walk must never do is read an address that is not mapped. A general-purpose
//! unwinder cannot know where the stack ends, so it has to guess or catch the fault. We know:
//! Shuttle allocated the task's stack, so we only read records that lie between the current stack
//! pointer and the stack's base, which is live, mapped memory. A link that leaves that range, or
//! does not move outwards, ends the walk.

use super::StackBounds;
use ::backtrace::{BacktraceFmt, BytesOrWideString, PrintFmt};
use std::arch::asm;
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt;
use std::path::Path;

thread_local! {
    /// The task stack this thread is running on, if any. See [`OnStack`].
    static CURRENT_STACK: Cell<Option<StackBounds>> = const { Cell::new(None) };
}

/// Marks the dynamic extent of a coroutine resume, during which this thread runs on the
/// coroutine's stack, so that a backtrace captured in the meantime knows which addresses it may read.
///
/// Only a backtrace reads the bounds, so this does nothing unless backtraces are enabled: it runs on
/// every resume, and the two thread-local accesses cost more than the check. The unit tests walk
/// task stacks without enabling backtraces, which are read from the environment once per process.
#[derive(Debug)]
pub(crate) struct OnStack {
    /// The stack to go back to on drop, if we entered one.
    restore: Option<Option<StackBounds>>,
}

impl OnStack {
    #[inline]
    pub(crate) fn enter(stack: StackBounds) -> Self {
        let enabled = cfg!(test) || crate::backtrace_enabled();
        Self {
            restore: enabled.then(|| CURRENT_STACK.replace(Some(stack))),
        }
    }
}

impl Drop for OnStack {
    #[inline]
    fn drop(&mut self) {
        if let Some(previous) = self.restore {
            CURRENT_STACK.set(previous);
        }
    }
}

/// Return addresses from a walk of a task stack, innermost first. Symbolized when printed.
pub(super) struct Frames(Vec<usize>);

/// Walk the current task stack, starting at the caller of the function this is inlined into, whose
/// own frame is skipped. `None` if we are not on a task stack.
#[inline(always)]
pub(super) fn capture() -> Option<Frames> {
    // Read the bounds here rather than in `walk`: on macOS a thread-local access is a call, and
    // `walk` must not make a call before it has read its return address out of `x30`.
    let stack = CURRENT_STACK.get()?;
    walk(stack).map(Frames)
}

/// The return address of each frame on the current stack, innermost first, starting with the one
/// our caller returns to: our caller's own frame is skipped. `None` if the current stack is not
/// `stack` after all, e.g. because user code switched to a stack of its own.
#[inline(never)]
fn walk(stack: StackBounds) -> Option<Vec<usize>> {
    let (fp, lr, sp): (usize, usize, usize);
    // SAFETY: copies three registers and touches nothing else. It has to come first, because `x30`
    // only holds our return address until we make a call.
    unsafe {
        asm!(
            "mov {fp}, x29",
            "mov {lr}, x30",
            "mov {sp}, sp",
            fp = out(reg) fp,
            lr = out(reg) lr,
            sp = out(reg) sp,
            options(nomem, nostack, preserves_flags),
        );
    }

    // Everything from the stack pointer up to the stack's base is live, mapped memory, so a frame
    // record that lies entirely within that range is safe to read. Nothing else is.
    if !(stack.limit < sp && sp < stack.base) {
        return None;
    }
    let readable = |record: usize| {
        let in_range = record >= sp && record.checked_add(16).is_some_and(|end| end <= stack.base);
        in_range && record.is_multiple_of(8)
    };
    if !readable(fp) {
        return None;
    }

    // `x29` is our own frame record, unless the compiler shrink-wrapped our prologue past the
    // `asm!` above, in which case it is already our caller's. Ours is the one that returns to `lr`.
    // Either way, start from our caller's, which returns to the first frame we want.
    // SAFETY: `fp` is readable.
    let mut record = if unsafe { read(fp + 8) } == lr {
        unsafe { read(fp) }
    } else {
        fp
    };

    let mut frames = Vec::with_capacity(32);
    while readable(record) {
        // SAFETY: `record` is readable.
        let (next, return_address) = unsafe { (read(record), read(record + 8)) };
        // Only a record that links to an outer one holds a return address. The one at the root of
        // the chain is corosensei's parent link at the base of the stack, whose second word is the
        // coroutine's entry point.
        if next <= record || !readable(next) {
            break;
        }
        frames.push(strip_pac(return_address));
        record = next;
    }

    (!frames.is_empty()).then_some(frames)
}

/// Read the word at `addr`.
///
/// # Safety
///
/// `addr` must be 8-byte aligned and mapped.
#[inline(always)]
unsafe fn read(addr: usize) -> usize {
    // Volatile because these are other functions' frames, whose contents the compiler knows nothing
    // about and must not reason about.
    (addr as *const usize).read_volatile()
}

/// Strip any pointer authentication code from a return address. Rust code on arm64 does not sign
/// return addresses, but system code built for arm64e does.
#[inline(always)]
fn strip_pac(mut address: usize) -> usize {
    // SAFETY: `xpaclri` only rewrites `x30`. It is spelled `hint #7` so that it assembles without
    // the `pauth` target feature, and it is a no-op on cores without pointer authentication.
    unsafe {
        asm!("hint #7", inout("x30") address, options(nomem, nostack, preserves_flags));
    }
    address
}

impl fmt::Display for Frames {
    /// Mirrors `Display for std::backtrace::Backtrace`, down to numbering each inlined function as
    /// a frame of its own. `{:#}` selects the full format, as it does there.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let style = if f.alternate() { PrintFmt::Full } else { PrintFmt::Short };
        let cwd = std::env::current_dir().ok();
        let mut print_path = move |f: &mut fmt::Formatter<'_>, path: BytesOrWideString<'_>| {
            write_path(f, &path.into_path_buf(), style, cwd.as_deref())
        };
        let mut out = BacktraceFmt::new(f, style, &mut print_path);
        out.add_context()?;
        for &return_address in &self.0 {
            let ip = return_address as *mut c_void;
            let mut symbolized = false;
            let mut result = Ok(());
            // `resolve` looks up the call instruction itself: it subtracts one from the address.
            ::backtrace::resolve(ip, |symbol| {
                symbolized = true;
                if result.is_ok() {
                    result = out.frame().print_raw_with_column(
                        ip,
                        symbol.name(),
                        symbol.filename_raw(),
                        symbol.lineno(),
                        symbol.colno(),
                    );
                }
            });
            result?;
            if !symbolized {
                out.frame().print_raw(ip, None, None, None)?;
            }
        }
        out.finish()
    }
}

impl fmt::Debug for Frames {
    /// Mirrors `Debug for std::backtrace::Backtrace`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cwd = std::env::current_dir().ok();
        let mut symbols = Vec::new();
        for &return_address in &self.0 {
            ::backtrace::resolve(return_address as *mut c_void, |symbol| {
                symbols.push(DebugSymbol {
                    name: symbol.name().map(|name| format!("{name:#}")),
                    file: symbol.filename().map(|path| {
                        let mut file = String::new();
                        // Writing to a `String` cannot fail.
                        let _ = write_path(&mut file, path, PrintFmt::Short, cwd.as_deref());
                        file
                    }),
                    line: symbol.lineno(),
                });
            });
        }
        write!(f, "Backtrace ")?;
        f.debug_list().entries(&symbols).finish()
    }
}

/// One symbol in the `Debug` output, formatted the way std formats a `BacktraceSymbol`.
struct DebugSymbol {
    name: Option<String>,
    file: Option<String>,
    line: Option<u32>,
}

impl fmt::Debug for DebugSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{{ fn: \"{name}\"")?,
            None => write!(f, "{{ fn: <unknown>")?,
        }
        if let Some(file) = &self.file {
            write!(f, ", file: \"{file}\"")?;
        }
        if let Some(line) = self.line {
            write!(f, ", line: {line}")?;
        }
        write!(f, " }}")
    }
}

/// Write a source path the way std's backtraces do: in the short format, relative to the working
/// directory when it lies under it.
fn write_path(out: &mut dyn fmt::Write, path: &Path, style: PrintFmt, cwd: Option<&Path>) -> fmt::Result {
    if style == PrintFmt::Short && path.is_absolute() {
        if let Some(relative) = cwd.and_then(|cwd| path.strip_prefix(cwd).ok()).and_then(Path::to_str) {
            return write!(out, ".{}{relative}", std::path::MAIN_SEPARATOR);
        }
    }
    write!(out, "{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::super::{Repr, TaskBacktrace};
    use super::*;
    use crate::config::Config;
    use crate::runtime::thread::continuation::Continuation;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Run `f` on a task stack, the way Shuttle runs a task, and return its result.
    fn on_task_stack<T: 'static>(f: impl FnOnce() -> T + 'static) -> T {
        let result = Rc::new(RefCell::new(None));
        let mut continuation = Continuation::new(Config::default().stack_size);
        let slot = Rc::clone(&result);
        continuation.initialize(Box::new(move || *slot.borrow_mut() = Some(f())));
        assert!(continuation.resume(), "the function should run to completion");
        let result = result.borrow_mut().take();
        result.expect("the function should have stored its result")
    }

    /// Walk the stack and unwind it with DWARF, from the same frame.
    #[inline(never)]
    fn walk_and_unwind() -> (Vec<usize>, Vec<usize>) {
        let walked = capture().expect("a task stack can be walked").0;
        let mut unwound = Vec::new();
        ::backtrace::trace(|frame| {
            unwound.push(frame.ip() as usize);
            true
        });
        (walked, unwound)
    }

    #[test]
    fn walk_matches_dwarf_unwind() {
        let (walked, unwound) = on_task_stack(walk_and_unwind);

        // The walk skips `walk_and_unwind`'s own frame, and the unwinder starts inside `trace`.
        // From the caller of `walk_and_unwind` onwards, they must see the same frames.
        let start = unwound
            .iter()
            .position(|&ip| ip == walked[0])
            .unwrap_or_else(|| panic!("walked {walked:x?} but unwound {unwound:x?}"));
        let end = start + walked.len();
        assert!(end <= unwound.len(), "walked {walked:x?} but unwound {unwound:x?}");
        assert_eq!(
            walked,
            unwound[start..end],
            "walked {walked:x?} but unwound {unwound:x?}"
        );

        // The walk stops at the base of the task stack, while the unwinder carries on into the stack
        // that resumed the task.
        assert!(end < unwound.len(), "walked {walked:x?} but unwound {unwound:x?}");
    }

    #[inline(never)]
    fn capture_here() -> TaskBacktrace {
        let backtrace = TaskBacktrace::capture();
        // Keep `capture` out of tail position, or `capture_here` would have no frame to report.
        std::hint::black_box(&backtrace);
        backtrace
    }

    #[test]
    fn walked_backtrace_prints_like_std() {
        const CALLER: &str = "shuttle_engine::runtime::task::backtrace::frame_records::tests::capture_here";

        let backtrace = on_task_stack(capture_here);
        assert!(matches!(backtrace.0, Repr::Walked(_)));

        // The first frame is the caller of `capture`, and, where there is debuginfo to say so, its
        // source location.
        let display = backtrace.to_string();
        let mut lines = display.lines();
        assert_eq!(lines.next(), Some(format!("   0: {CALLER}").as_str()), "{display}");
        if cfg!(debug_assertions) {
            let location = lines.next().unwrap_or_default();
            assert!(
                location.starts_with("             at ") && location.contains("frame_records.rs:"),
                "{display}"
            );
        }

        let debug = format!("{backtrace:?}");
        assert!(debug.starts_with(&format!("Backtrace [{{ fn: \"{CALLER}\"")), "{debug}");
    }

    #[test]
    fn falls_back_to_std_off_a_task_stack() {
        // Not on any task stack.
        assert!(matches!(capture_here().0, Repr::Std(_)));

        // Claiming to be on a stack we are not on.
        let _on_stack = OnStack::enter(StackBounds {
            limit: 0x1000,
            base: 0x2000,
        });
        assert!(capture().is_none());
        assert!(matches!(capture_here().0, Repr::Std(_)));
    }
}
