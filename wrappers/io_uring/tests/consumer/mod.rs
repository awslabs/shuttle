//! Code written against io-uring, as a crate that depends on `shuttle-io-uring`
//! would have it: it names `io_uring::...` and nothing else. The same source is
//! compiled against `shuttle-io-uring` without the `shuttle` feature, where it
//! drives the kernel's ring, and against `shuttle-io-uring-impl`, which is what
//! the crate is under `shuttle`, where it runs under Shuttle. The test that
//! includes it names the crate `crate::io_uring`.
//!
//! The checks hold for the kernel and the simulation alike, so they assume
//! nothing about when an op completes, and they use only what kernel 5.10 has.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

use crate::io_uring::types::{Fd, Fixed, SubmitArgs, Timespec};
use crate::io_uring::{IoUring, Probe, cqueue, opcode, squeue};

const BLOCK: usize = 4096;

/// Exercises the API.
pub fn exercise() {
    let file = tempfile::tempfile().expect("temp file");
    vectored_round_trip(&file);
    single_buffer_ops(&file);
    fixed_files(&file);
    timeout_and_cancel();
    probe_and_params();
}

/// Push an entry, submitting to make room while the queue is full, as a ring
/// thread typically does.
fn push(ring: &mut IoUring, entry: &squeue::Entry) {
    // SAFETY: the callers keep the entry's buffers alive until its completion
    // is reaped.
    while unsafe { ring.submission().push(entry) }.is_err() {
        ring.submit().expect("submit");
    }
}

/// Reap `n` completions, waiting for them with bounded waits as a ring thread
/// typically does, and return them sorted by `user_data`.
fn wait_for(ring: &mut IoUring, n: usize) -> Vec<(u64, i32)> {
    let mut done = Vec::new();
    let ts = Timespec::new().sec(0).nsec(100_000_000);
    let args = SubmitArgs::new().timespec(&ts);
    let ext_arg = ring.params().is_feature_ext_arg();
    loop {
        done.extend(
            ring.completion()
                .map(|cqe: cqueue::Entry| (cqe.user_data(), cqe.result())),
        );
        if done.len() >= n {
            break;
        }
        let entered = if ext_arg {
            ring.submitter().submit_with_args(1, &args)
        } else {
            ring.submit_and_wait(1)
        };
        match entered {
            Ok(_) => {}
            Err(e) if matches!(e.raw_os_error(), Some(libc::ETIME | libc::EINTR)) => {}
            Err(e) => panic!("enter failed: {e}"),
        }
    }
    done.sort();
    done
}

/// Writes four blocks with `Writev` and reads them back with `Readv`, one
/// `iovec` per op.
fn vectored_round_trip(file: &File) {
    let mut ring = IoUring::new(8).expect("ring");
    let blocks: Vec<Vec<u8>> = (1..=4_u8).map(|i| vec![i; BLOCK]).collect();
    let writes: Vec<libc::iovec> = blocks
        .iter()
        .map(|block| libc::iovec {
            iov_base: block.as_ptr() as *mut libc::c_void,
            iov_len: block.len(),
        })
        .collect();
    for (i, iovec) in writes.iter().enumerate() {
        let entry = opcode::Writev::new(Fd(file.as_raw_fd()), iovec, 1)
            .offset((i * BLOCK) as u64)
            .rw_flags(0)
            .build()
            .user_data(i as u64);
        push(&mut ring, &entry);
    }
    ring.submit().expect("submit");
    let expected: Vec<(u64, i32)> = (0..4).map(|i| (i, BLOCK as i32)).collect();
    assert_eq!(wait_for(&mut ring, 4), expected);

    let mut reads = vec![vec![0_u8; BLOCK]; blocks.len()];
    let iovecs: Vec<libc::iovec> = reads
        .iter_mut()
        .map(|read| libc::iovec {
            iov_base: read.as_mut_ptr().cast(),
            iov_len: read.len(),
        })
        .collect();
    for (i, iovec) in iovecs.iter().enumerate() {
        let entry = opcode::Readv::new(Fd(file.as_raw_fd()), iovec, 1)
            .offset((i * BLOCK) as u64)
            .build()
            .user_data(i as u64);
        push(&mut ring, &entry);
    }
    ring.submit().expect("submit");
    assert_eq!(wait_for(&mut ring, 4), expected);
    assert_eq!(reads, blocks);
}

/// `Write`, `Fsync`, `Fallocate`, `Read` and `Nop`, one after the other.
fn single_buffer_ops(file: &File) {
    let mut ring = IoUring::builder().setup_cqsize(16).build(4).expect("ring");
    let data = vec![7_u8; BLOCK];
    let offset = (8 * BLOCK) as u64;
    let write = opcode::Write::new(Fd(file.as_raw_fd()), data.as_ptr(), BLOCK as u32)
        .offset(offset)
        .build()
        .user_data(1);
    push(&mut ring, &write);
    assert_eq!(ring.submit_and_wait(1).expect("enter"), 1);
    assert_eq!(wait_for(&mut ring, 1), [(1, BLOCK as i32)]);

    push(
        &mut ring,
        &opcode::Fsync::new(Fd(file.as_raw_fd())).build().user_data(2),
    );
    push(
        &mut ring,
        &opcode::Fallocate::new(Fd(file.as_raw_fd()), BLOCK as u64)
            .offset(offset + BLOCK as u64)
            .build()
            .user_data(3),
    );
    push(&mut ring, &opcode::Nop::new().build().user_data(4));
    ring.submit().expect("submit");
    let done = wait_for(&mut ring, 3);
    assert_eq!(done[0], (2, 0), "fsync");
    assert!(
        done[1] == (3, 0) || done[1] == (3, -libc::EOPNOTSUPP),
        "fallocate: {:?}",
        done[1]
    );
    assert_eq!(done[2], (4, 0), "nop");

    let mut buf = vec![0_u8; BLOCK];
    let read = opcode::Read::new(Fd(file.as_raw_fd()), buf.as_mut_ptr(), BLOCK as u32)
        .offset(offset)
        .build()
        .user_data(5);
    push(&mut ring, &read);
    ring.submit().expect("submit");
    assert_eq!(wait_for(&mut ring, 1), [(5, BLOCK as i32)]);
    assert_eq!(buf, data);
}

/// A write through the registered-file table.
fn fixed_files(file: &File) {
    let mut ring = IoUring::new(8).expect("ring");
    ring.submitter()
        .register_files(&[file.as_raw_fd()])
        .expect("register files");
    let data = [9_u8; 512];
    let offset = (16 * BLOCK) as u64;
    let write = opcode::Write::new(Fixed(0), data.as_ptr(), data.len() as u32)
        .offset(offset)
        .build()
        .user_data(1);
    push(&mut ring, &write);
    ring.submit().expect("submit");
    assert_eq!(wait_for(&mut ring, 1), [(1, 512)]);
    let mut read = [0_u8; 512];
    file.read_exact_at(&mut read, offset).expect("read back");
    assert_eq!(read, data);
    ring.submitter().unregister_files().expect("unregister files");
}

/// A long timeout, and a cancel of it. The cancel either cancels it, or finds
/// it past cancelling (`EALREADY`, which only the simulation chooses) and lets
/// it fire.
fn timeout_and_cancel() {
    let mut ring = IoUring::new(8).expect("ring");
    let ts = Timespec::new().sec(60);
    push(&mut ring, &opcode::Timeout::new(&ts).build().user_data(1));
    push(&mut ring, &opcode::AsyncCancel::new(1).build().user_data(2));
    ring.submit().expect("submit");
    match wait_for(&mut ring, 2)[..] {
        [(1, timeout), (2, cancel)] => match cancel {
            0 => assert_eq!(timeout, -libc::ECANCELED),
            c if c == -libc::EALREADY => assert_eq!(timeout, -libc::ETIME),
            other => panic!("cancel result {other}"),
        },
        ref other => panic!("completions {other:?}"),
    }
}

/// What the ring reports about itself.
fn probe_and_params() {
    let ring = IoUring::new(8).expect("ring");
    assert_eq!((ring.params().sq_entries(), ring.params().cq_entries()), (8, 16));
    let mut probe = Probe::new();
    ring.submitter().register_probe(&mut probe).expect("probe");
    for code in [
        opcode::Nop::CODE,
        opcode::Readv::CODE,
        opcode::Writev::CODE,
        opcode::Fsync::CODE,
        opcode::Read::CODE,
        opcode::Write::CODE,
    ] {
        assert!(probe.is_supported(code), "opcode {code}");
    }
}
