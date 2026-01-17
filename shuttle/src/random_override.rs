//! Derived from <https://github.com/s2-streamstore/mad-turmoil/blob/main/src/rand.rs>

use crate::rand::RngCore;
use std::cell::Cell;
use std::fs::File;
use std::io::Read;
use std::io;

pub struct ShuttleRandomGuard {
    // Just to make it unconstructible without going through `set_rng`
    _field: (),
}

impl Drop for ShuttleRandomGuard {
    fn drop(&mut self) {
        println!("DROP");
        RNG_INITED.set(None);
    }
}


thread_local! {
    static RNG_INITED: Cell<Option<()>> = Cell::new(None);
}

#[must_use = "The Shuttle-compatible random will be unset once the `ShuttleRandomGuard` is dropped"]
pub fn set_rng() -> ShuttleRandomGuard {
    println!("INIT");
    RNG_INITED
        .set(Some(()));
    ShuttleRandomGuard { _field: () }
}

fn try_rng() -> Option<crate::rand::rngs::ThreadRng> {
    println!("try_rng");
    RNG_INITED.get().map(|_| crate::rand::thread_rng() )
}

fn fill_with_dev_urandom(dest: &mut [u8]) -> io::Result<()> {
    println!("dev urandom");
    for e in dest {
        *e = 0;
    }
    //let mut file = File::open("/dev/urandom")?;
    //file.read_exact(dest)?;
    Ok(())
}


#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn getrandom(buf: *mut u8, buflen: usize, _flags: u32) -> isize {
    println!("getrandom");
    // <https://man7.org/linux/man-pages/man2/getrandom.2.html>
    if !buf.is_null() && buflen > 0 {
        let dest = unsafe { std::slice::from_raw_parts_mut(buf, buflen) };
        match try_rng() {
            Some(mut rng) => {
                //panic!();
                println!("Filling bytes: {:?}", dest);
                let rand = crate::rand::thread_rng();
                rng.fill_bytes(dest);
                println!("Filling bytes: {:?}", dest);
                /*
                if fill_with_dev_urandom(dest).is_err() {
                    return -1;
                }
                */
            }
            None => {
                println!("NONE");
                // for call sites (e.g. test runner, getting random seed if not set in env var)
                // before the test has set the RNG.
                if fill_with_dev_urandom(dest).is_err() {
                    return -1;
                }
            }
        }
        buflen as isize
    } else {
        panic!();
        -1
    }
}

#[unsafe(no_mangle)]
#[cfg(target_os = "macos")]
#[inline(never)]
unsafe extern "C" fn CCRandomGenerateBytes(buf: *mut u8, buflen: usize) -> i32 {
    println!("CC");
    // For Mac OS
    // - <https://blog.xoria.org/randomness-on-apple-platforms/>
    // - <https://linear.app/streamstore/issue/S2-597/fix-non-determinism-in-dst>
    if unsafe { getrandom(buf, buflen, 0) } as i32 != -1 {
        0
    } else {
        -1
    }
}

#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn getentropy(buf: *mut u8, buflen: usize) -> i32 {
    println!("getentropy");
    // <https://man7.org/linux/man-pages/man3/getentropy.3.html>
    if buflen > 256 {
        return -1;
    }
    match unsafe { getrandom(buf, buflen, 0) } {
        -1 => -1,
        _ => 0,
    }
}