//! Frame-pointer backtraces for arm64 and x86_64, on macOS and Linux.
//!
//! A function that keeps a frame pointer stores a *frame record* on the stack: two words holding
//! its caller's frame pointer and its own return address, with the frame pointer register (`x29`
//! or `rbp`) pointing at it. The records form a linked list from the innermost frame outwards, so a
//! backtrace is two loads per frame. Symbolizing the return addresses is the expensive part, and
//! waits until the backtrace is printed.
//!
//! Whether every frame keeps a record depends on the build. Apple's ABIs require it, and Rust keeps
//! them by default on arm64 Linux, but x86_64 Linux omits them unless built with
//! `-C force-frame-pointers=yes`. A frame without a record either breaks the chain or silently drops
//! out of it, so the first walk in a process is checked against a DWARF unwind of the same stack,
//! and if they disagree we use std's backtrace for the rest of the process.
//!
//! What a frame-pointer walk must never do is read an address that is not mapped. A general-purpose
//! unwinder cannot know where the stack ends, so it has to guess or catch the fault. We know:
//! Shuttle allocated the task's stack, so we only read records that lie between the current stack
//! pointer and the stack's base, which is live, mapped memory. A link that leaves that range, or
//! does not move outwards, ends the walk, and a walk that ends anywhere but the root record at the
//! base of the stack is discarded.

use super::StackBounds;
use crate::runtime::execution::ExecutionState;
use ::backtrace::{BacktraceFmt, BytesOrWideString, PrintFmt};
use owo_colors::OwoColorize;
use std::arch::asm;
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

const WORD: usize = std::mem::size_of::<usize>();

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

/// Whether frame-pointer walks can be trusted in this process. It only ever increases, so that
/// once one check has failed, no later one can bring the walks back.
static TRUST: AtomicU8 = AtomicU8::new(UNCHECKED);
/// No walk has reached a verdict yet.
const UNCHECKED: u8 = 0;
/// The first walk agreed with a DWARF unwind of the same stack.
const TRUSTED: u8 = 1;
/// The first walk did not, so this build omits frame pointers, at least in places.
const DISTRUSTED: u8 = 2;

/// Return addresses from a walk of a task stack, innermost first. Symbolized when printed.
pub(super) struct Frames(Vec<usize>);

/// Walk the current task stack, starting with the return address of the function this is inlined
/// into. `None` if we are not on a task stack, or cannot trust the walk.
#[inline(always)]
pub(super) fn capture() -> Option<Frames> {
    let stack = CURRENT_STACK.get()?;
    let trust = TRUST.load(Ordering::Relaxed);
    if trust == DISTRUSTED {
        return None;
    }
    match walk(stack, trust == UNCHECKED) {
        Walk::OffStack => None,
        Walk::Complete(frames) => {
            if trust == UNCHECKED {
                TRUST.fetch_max(TRUSTED, Ordering::Relaxed);
            }
            Some(Frames(frames))
        }
        // After the first walk, a broken one is a one-off, like a frame from C code, so it only
        // costs this backtrace.
        Walk::Broken | Walk::Disagrees => {
            if trust == UNCHECKED {
                distrust();
            }
            None
        }
    }
}

/// How a walk of the current stack ended.
enum Walk {
    /// The stack pointer is not on the task's stack: we are on the executor's, or user code
    /// switched to a stack of its own.
    OffStack,
    /// The chain of records broke before it reached the root: some frame on the stack keeps no
    /// record, or uses the frame pointer register for something else.
    Broken,
    /// The chain reached the root, but a DWARF unwind of the same stack found different frames.
    Disagrees,
    /// The chain reached the root, and these are its return addresses, innermost first.
    Complete(Vec<usize>),
}

/// Walk the current stack's frame records, starting with our caller's return address, and, if
/// `check`, compare the result against a DWARF unwind.
#[inline(never)]
fn walk(stack: StackBounds, check: bool) -> Walk {
    // Our own frame record need not exist before we make a call: the compiler can move our prologue
    // past code that does not use the stack. The allocation is a call, so read the registers after
    // it.
    let mut frames = Vec::with_capacity(32);
    let (fp, sp) = frame_and_stack_pointers(frames.as_ptr());

    // Everything from the stack pointer up to the stack's base is live, mapped memory, so a frame
    // record that lies entirely within that range is safe to read. Nothing else is.
    if !(stack.limit < sp && sp < stack.base) {
        return Walk::OffStack;
    }
    let readable = |record: usize| {
        let in_range = record >= sp && record.checked_add(2 * WORD).is_some_and(|end| end <= stack.base);
        in_range && (record & (WORD - 1)) == 0
    };
    if !readable(fp) {
        return Walk::Broken;
    }

    // Skip our own record, which returns into our caller, and start from our caller's.
    // SAFETY: `fp` is readable.
    let mut record = unsafe { read(fp) };
    while readable(record) {
        // SAFETY: `record` is readable.
        let (next, return_address) = unsafe { (read(record), read(record + WORD)) };
        // Only a record that links to an outer one holds a return address. The root's second word
        // is the coroutine's entry point.
        if next <= record || !readable(next) {
            break;
        }
        frames.push(strip_pac(return_address));
        record = next;
    }

    if record != stack.root || frames.is_empty() {
        Walk::Broken
    } else if check && !agrees_with_dwarf(&frames) {
        Walk::Disagrees
    } else {
        Walk::Complete(frames)
    }
}

/// Whether `frames`, from a walk further up this stack, are a run of the frames a DWARF unwind finds.
///
/// A frame without a record does not always break the chain of records: if it leaves the frame
/// pointer register alone, its callee links straight to its caller's record, and the frame drops
/// out of the walk without a trace. Only an unwinder that does not rely on frame records can tell.
/// Checking once per process is enough, because which frames keep records is a property of the
/// build.
#[cold]
#[inline(never)]
fn agrees_with_dwarf(frames: &[usize]) -> bool {
    let mut unwound = Vec::new();
    ::backtrace::trace(|frame| {
        unwound.push(frame.ip() as usize);
        true
    });
    // The unwinder starts in here, reaches the frame the walk started at a few frames later, and
    // carries on past the base of the task stack.
    unwound.windows(frames.len()).any(|window| window == frames)
}

/// Stop walking frame pointers in this process, and say why, once.
#[cold]
fn distrust() {
    if TRUST.fetch_max(DISTRUSTED, Ordering::Relaxed) == DISTRUSTED {
        return;
    }
    // Only `try_with`: we may be capturing from inside a borrow of the execution state.
    let silenced =
        crate::silence_warnings() || ExecutionState::try_with(|state| state.config.silence_warnings).unwrap_or(false);
    if !silenced {
        eprintln!(
            "{}: {} is set, but this build does not keep a frame pointer in every frame, so Shuttle \
            captures backtraces with std::backtrace instead, which is much slower. Build with \
            RUSTFLAGS=\"-C force-frame-pointers=yes\" to make capturing them fast.",
            "WARNING".yellow(),
            crate::CAPTURE_BACKTRACE,
        );
    }
}

/// The frame pointer and the stack pointer. `after` is not used, except to keep the compiler from
/// reading them before whatever produced it.
#[inline(always)]
fn frame_and_stack_pointers<T>(after: *const T) -> (usize, usize) {
    let (fp, sp): (usize, usize);
    // SAFETY: copies two registers and touches nothing else.
    unsafe {
        #[cfg(target_arch = "aarch64")]
        asm!(
            "mov {fp}, x29",
            "mov {sp}, sp",
            fp = inout(reg) after as usize => fp,
            sp = out(reg) sp,
            options(nomem, nostack, preserves_flags),
        );
        #[cfg(target_arch = "x86_64")]
        asm!(
            "mov {fp}, rbp",
            "mov {sp}, rsp",
            fp = inout(reg) after as usize => fp,
            sp = out(reg) sp,
            options(nomem, nostack, preserves_flags),
        );
    }
    (fp, sp)
}

/// Read the word at `addr`.
///
/// # Safety
///
/// `addr` must be word-aligned and mapped.
#[inline(always)]
unsafe fn read(addr: usize) -> usize {
    // Volatile because these are other functions' frames, whose contents the compiler knows nothing
    // about and must not reason about.
    (addr as *const usize).read_volatile()
}

/// Strip any pointer authentication code from a return address. Rust code on arm64 does not sign
/// return addresses, but system code built for arm64e does.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn strip_pac(mut address: usize) -> usize {
    // SAFETY: `xpaclri` only rewrites `x30`. It is spelled `hint #7` so that it assembles without
    // the `pauth` target feature, and it is a no-op on cores without pointer authentication.
    unsafe {
        asm!("hint #7", inout("x30") address, options(nomem, nostack, preserves_flags));
    }
    address
}

/// x86_64 has no pointer authentication.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn strip_pac(address: usize) -> usize {
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

    /// Whether this build keeps a frame pointer in every frame. Apple's ABIs require it, and Rust
    /// does it by default on arm64 Linux. On x86_64 Linux it takes `-C force-frame-pointers`, which
    /// we can only see if it came from `RUSTFLAGS`.
    fn frame_pointers_expected() -> bool {
        cfg!(any(target_vendor = "apple", target_arch = "aarch64"))
            || option_env!("RUSTFLAGS").is_some_and(|flags| flags.contains("force-frame-pointers"))
    }

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

    fn current_stack() -> StackBounds {
        CURRENT_STACK.get().expect("should be running on a task stack")
    }

    /// Walk the stack, without checking the walk, and unwind it with DWARF, from the same function.
    #[inline(never)]
    fn walk_and_unwind() -> (Walk, Vec<usize>) {
        let walked = walk(current_stack(), false);
        let mut unwound = Vec::new();
        ::backtrace::trace(|frame| {
            unwound.push(frame.ip() as usize);
            true
        });
        (walked, unwound)
    }

    /// Whether a walk from `walk_and_unwind` found the same frames as its DWARF unwind. The walk
    /// starts with `walk_and_unwind`'s return address, and the unwinder inside `trace`, so the walk
    /// must be a run of the unwinder's frames. The unwinder must also carry on past the base of the
    /// task stack, where the walk stops.
    fn agrees(walked: &[usize], unwound: &[usize]) -> bool {
        (0..unwound.len().saturating_sub(walked.len())).any(|start| unwound[start..start + walked.len()] == *walked)
    }

    #[test]
    fn walk_matches_dwarf_unwind() {
        let (result, unwound) = on_task_stack(walk_and_unwind);
        if !frame_pointers_expected() {
            // This build may omit frame pointers, in which case there is nothing to match.
            // `capture_uses_the_walk_only_if_dwarf_agrees` checks that we notice.
            return;
        }
        let Walk::Complete(walked) = result else {
            panic!("the walk should reach the base of the task stack");
        };
        assert!(agrees(&walked, &unwound), "walked {walked:x?} but unwound {unwound:x?}");

        // And so does the check that `capture` makes.
        let checked = on_task_stack(|| walk(current_stack(), true));
        assert!(matches!(checked, Walk::Complete(_)));
    }

    #[inline(never)]
    fn capture_here() -> TaskBacktrace {
        let backtrace = TaskBacktrace::capture();
        // Keep `capture` out of tail position, or `capture_here` would have no frame to report.
        std::hint::black_box(&backtrace);
        backtrace
    }

    #[test]
    fn capture_uses_the_walk_only_if_dwarf_agrees() {
        let (result, unwound) = on_task_stack(walk_and_unwind);
        let walk_agrees = matches!(&result, Walk::Complete(walked) if agrees(walked, &unwound));
        let walked = matches!(on_task_stack(capture_here).0, Repr::Walked(_));
        assert_eq!(walked, walk_agrees);
    }

    #[test]
    fn walked_backtrace_prints_like_std() {
        const CALLER: &str = "shuttle_engine::runtime::task::backtrace::frame_records::tests::capture_here";

        let backtrace = on_task_stack(capture_here);
        if !matches!(backtrace.0, Repr::Walked(_)) {
            assert!(
                !frame_pointers_expected(),
                "a build with frame pointers should walk them"
            );
            return;
        }

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
        let _on_stack = OnStack::enter(StackBounds::new(0x1000, 0x2000, 0x1ff0));
        assert!(capture().is_none());
        assert!(matches!(capture_here().0, Repr::Std(_)));
    }
}
