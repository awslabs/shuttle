//! Generating UUIDs from timestamps: uuid's API, with the fake clock and random bits from Shuttle
//! (see the crate documentation).
//!
//! `Timestamp` and the contexts are this crate's own, ported from uuid 1.29.2 (Copyright 2013-2014
//! The Rust Project Developers, 2018 The Uuid Project Developers; Apache-2.0 OR MIT). uuid keeps
//! a timestamp's counter private, and `Uuid::new_v7` needs it, so `Timestamp` can't wrap uuid's.

pub use uuid_orig::timestamp::{ClockSequence, UUID_TICKS_BETWEEN_EPOCHS};

/// A timestamp that can be encoded into a UUID: uuid's [`Timestamp`](uuid_orig::Timestamp),
/// except that [`Timestamp::now`] reads the fake clock (see the crate documentation).
///
/// It converts into uuid's `Timestamp` with `.into()`, but not back: uuid doesn't expose a
/// timestamp's counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
    unix_nanos: u128,
    counter: u128,
    usable_counter_bits: u8,
}

impl Timestamp {
    /// Get a timestamp representing the current time, from the fake clock, with a counter from
    /// `context`.
    ///
    /// # Panics
    ///
    /// Panics outside of a Shuttle test.
    #[cfg(feature = "std")]
    pub fn now(context: impl ClockSequence<Output = impl Into<u128>>) -> Self {
        let (seconds, subsec_nanos) = crate::clock::now();
        Timestamp::from_unix(context, seconds, subsec_nanos)
    }

    /// Construct a `Timestamp` from the number of 100 nanosecond ticks since 00:00:00.00,
    /// 15 October 1582 (the date of Gregorian reform to the Christian calendar) and a 14-bit
    /// counter, as used in versions 1 and 6 UUIDs.
    pub const fn from_gregorian_time(ticks: u64, counter: u16) -> Self {
        let (seconds, subsec_nanos) = gregorian_to_unix(ticks);

        Timestamp {
            unix_nanos: parts_to_nanos(seconds, subsec_nanos),
            counter: counter as u128,
            usable_counter_bits: 14,
        }
    }

    /// Construct a `Timestamp` from a Unix timestamp and sequence number.
    pub const fn from_unix_time(seconds: u64, subsec_nanos: u32, counter: u128, usable_counter_bits: u8) -> Self {
        Timestamp {
            unix_nanos: parts_to_nanos(seconds, subsec_nanos),
            counter,
            usable_counter_bits,
        }
    }

    /// Construct a `Timestamp` from a Unix timestamp and a counter from `context`.
    pub fn from_unix(context: impl ClockSequence<Output = impl Into<u128>>, seconds: u64, subsec_nanos: u32) -> Self {
        let (counter, seconds, subsec_nanos) = context.generate_timestamp_sequence(seconds, subsec_nanos);
        let counter = counter.into();
        let usable_counter_bits = context.usable_bits() as u8;

        Timestamp {
            unix_nanos: parts_to_nanos(seconds, subsec_nanos),
            counter,
            usable_counter_bits,
        }
    }

    /// Get the value of the timestamp as the number of 100 nanosecond ticks since 00:00:00.00,
    /// 15 October 1582 and a 14-bit counter, as used in versions 1 and 6 UUIDs.
    pub const fn to_gregorian(&self) -> (u64, u16) {
        let (seconds, subsec_nanos) = nanos_to_parts(self.unix_nanos);

        (
            unix_to_gregorian_ticks(seconds, subsec_nanos),
            (self.counter as u16) & 0x3FFF,
        )
    }

    /// Get the value of the timestamp as a Unix timestamp, consisting of the number of whole and
    /// fractional seconds.
    pub const fn to_unix(&self) -> (u64, u32) {
        nanos_to_parts(self.unix_nanos)
    }

    /// The counter, and how many of its bits are used, for `Uuid::new_v7`.
    #[cfg(feature = "v7")]
    pub(crate) const fn counter(&self) -> (u128, u8) {
        (self.counter, self.usable_counter_bits)
    }
}

#[doc(hidden)]
impl Timestamp {
    #[deprecated(since = "1.10.0", note = "use `Timestamp::from_gregorian_time(ticks, counter)`")]
    pub const fn from_rfc4122(ticks: u64, counter: u16) -> Self {
        Timestamp::from_gregorian_time(ticks, counter)
    }

    #[deprecated(since = "1.10.0", note = "use `Timestamp::to_gregorian()`")]
    pub const fn to_rfc4122(&self) -> (u64, u16) {
        self.to_gregorian()
    }

    #[deprecated(
        since = "1.2.0",
        note = "`Timestamp::to_unix_nanos()` is deprecated and will be removed: use `Timestamp::to_unix()`"
    )]
    pub const fn to_unix_nanos(&self) -> u32 {
        panic!("`Timestamp::to_unix_nanos()` is deprecated and will be removed: use `Timestamp::to_unix()`")
    }

    #[deprecated(since = "1.23.0", note = "use `Timestamp::from_gregorian_time(ticks, counter)`")]
    pub const fn from_gregorian(ticks: u64, counter: u16) -> Self {
        Timestamp::from_gregorian_time(ticks, counter)
    }
}

#[cfg(feature = "std")]
impl TryFrom<std::time::SystemTime> for Timestamp {
    type Error = crate::Error;

    fn try_from(st: std::time::SystemTime) -> Result<Self, Self::Error> {
        // uuid makes this timestamp without a counter.
        uuid_orig::Timestamp::try_from(st).map(|ts| {
            let (seconds, subsec_nanos) = ts.to_unix();
            Timestamp::from_unix_time(seconds, subsec_nanos, 0, 0)
        })
    }
}

#[cfg(feature = "std")]
impl From<Timestamp> for std::time::SystemTime {
    fn from(ts: Timestamp) -> Self {
        uuid_orig::Timestamp::from(ts).into()
    }
}

impl From<Timestamp> for uuid_orig::Timestamp {
    fn from(ts: Timestamp) -> Self {
        let (seconds, subsec_nanos) = ts.to_unix();
        uuid_orig::Timestamp::from_unix_time(seconds, subsec_nanos, ts.counter, ts.usable_counter_bits)
    }
}

const fn unix_to_gregorian_ticks(seconds: u64, nanos: u32) -> u64 {
    UUID_TICKS_BETWEEN_EPOCHS
        .wrapping_add(seconds.wrapping_mul(10_000_000))
        .wrapping_add(nanos as u64 / 100)
}

const fn gregorian_to_unix(ticks: u64) -> (u64, u32) {
    (
        ticks.wrapping_sub(UUID_TICKS_BETWEEN_EPOCHS) / 10_000_000,
        (ticks.wrapping_sub(UUID_TICKS_BETWEEN_EPOCHS) % 10_000_000) as u32 * 100,
    )
}

/// Converts 128bit nanoseconds into 64bit seconds and 32bit subsecond nanos, as uuid does: every
/// value that `parts_to_nanos` produces converts back exactly.
const fn nanos_to_parts(nanos: u128) -> (u64, u32) {
    const MAX_NANOS: u128 = parts_to_nanos(u64::MAX, u32::MAX);

    if nanos > MAX_NANOS {
        return (u64::MAX, u32::MAX);
    }

    let secs = nanos / 1_000_000_000;

    if secs > u64::MAX as u128 {
        (u64::MAX, (nanos - u64::MAX as u128 * 1_000_000_000) as u32)
    } else {
        (secs as u64, (nanos % 1_000_000_000) as u32)
    }
}

const fn parts_to_nanos(secs: u64, subsec_nanos: u32) -> u128 {
    (secs as u128 * 1_000_000_000) + subsec_nanos as u128
}

/// Contexts for use with time-based UUIDs.
pub mod context {
    pub use uuid_orig::timestamp::context::NoContext;

    #[cfg(feature = "std")]
    pub use uuid_orig::timestamp::context::ThreadLocalContext;

    #[cfg(any(feature = "v1", feature = "v6"))]
    pub use v1_support::*;

    #[cfg(feature = "v7")]
    pub use v7_support::*;

    #[cfg(any(feature = "v1", feature = "v6"))]
    mod v1_support {
        use super::super::ClockSequence;
        use core::sync::atomic::{AtomicU16, Ordering};

        /// A thread-safe, wrapping counter that produces 14-bit values for version 1 and version 6
        /// UUIDs: uuid's [`ContextV1`](uuid_orig::ContextV1), whose random start comes from
        /// Shuttle (see the crate documentation).
        #[derive(Debug)]
        pub struct ContextV1 {
            count: AtomicU16,
        }

        impl ContextV1 {
            /// Construct a new context that's initialized with the given value.
            pub const fn new(count: u16) -> Self {
                Self {
                    count: AtomicU16::new(count),
                }
            }

            /// Construct a new context that's initialized with a random value from Shuttle (see the
            /// crate documentation).
            ///
            /// # Panics
            ///
            /// Panics outside of a Shuttle test.
            #[cfg(feature = "rng")]
            pub fn new_random() -> Self {
                Self::new(crate::rng::u16())
            }

            /// The value that the next sequence starts from.
            #[cfg(all(feature = "std", feature = "rng"))]
            pub(crate) fn count(&self) -> u16 {
                self.count.load(Ordering::Relaxed)
            }
        }

        impl ClockSequence for ContextV1 {
            type Output = u16;

            fn generate_sequence(&self, _seconds: u64, _nanos: u32) -> Self::Output {
                // As in uuid: RFC 9562 reserves 2 bits of the clock sequence, so wrap at 14 bits.
                self.count.fetch_add(1, Ordering::AcqRel) & (u16::MAX >> 2)
            }

            fn usable_bits(&self) -> usize {
                14
            }
        }

        #[deprecated(since = "1.23.0", note = "renamed to `ContextV1`")]
        #[doc(hidden)]
        pub type Context = ContextV1;
    }

    #[cfg(feature = "v7")]
    mod v7_support {
        use super::super::{ClockSequence, parts_to_nanos};
        use core::{cell::Cell, cmp, panic::RefUnwindSafe};

        const USABLE_BITS: usize = 42;

        // Leave the most significant bit unset, so that the counter can't overflow in practice.
        const RESEED_MASK: u64 = u64::MAX >> 23;
        const MAX_COUNTER: u64 = u64::MAX >> 22;

        /// An unsynchronized, reseeding counter that produces 42-bit values for version 7 UUIDs:
        /// uuid's [`ContextV7`](uuid_orig::ContextV7), whose counter is reseeded with random bits
        /// from Shuttle (see the crate documentation).
        ///
        /// Reseeding panics outside of a Shuttle test.
        #[derive(Debug, Default)]
        pub struct ContextV7 {
            timestamp: Cell<ReseedingTimestamp>,
            counter: Cell<Counter>,
            adjust: Adjust,
            precision: Precision,
        }

        impl RefUnwindSafe for ContextV7 {}

        impl ContextV7 {
            /// Construct a new context that will reseed its counter on the first non-zero timestamp
            /// it receives.
            pub const fn new() -> Self {
                ContextV7 {
                    timestamp: Cell::new(ReseedingTimestamp {
                        last_seed: 0,
                        unix_nanos: 0,
                    }),
                    counter: Cell::new(Counter { value: 0 }),
                    adjust: Adjust { by_ns: 0 },
                    precision: Precision {
                        bits: 0,
                        mask: 0,
                        factor: 0,
                        shift: 0,
                    },
                }
            }

            /// Specify an amount to shift timestamps by to obfuscate their actual generation time.
            pub fn with_adjust_by_millis(mut self, millis: u32) -> Self {
                self.adjust = Adjust::by_millis(millis);
                self
            }

            /// Use the leftmost 12 bits of the counter for additional timestamp precision.
            pub fn with_additional_precision(self) -> Self {
                self.with_additional_precision_bits(12)
            }

            /// Use the leftmost `bits` bits of the counter for additional timestamp precision.
            pub fn with_additional_precision_bits(mut self, bits: usize) -> Self {
                self.precision = Precision::new(cmp::min(bits, 12));
                self
            }

            /// A copy of the context, which `now_v7` generates from outside of the execution's
            /// storage, because reseeding takes a random value from Shuttle.
            #[cfg(feature = "std")]
            pub(crate) fn copy(&self) -> Self {
                ContextV7 {
                    timestamp: Cell::new(self.timestamp.get()),
                    counter: Cell::new(self.counter.get()),
                    adjust: self.adjust,
                    precision: self.precision,
                }
            }
        }

        impl ClockSequence for ContextV7 {
            type Output = u64;

            fn generate_sequence(&self, seconds: u64, subsec_nanos: u32) -> Self::Output {
                self.generate_timestamp_sequence(seconds, subsec_nanos).0
            }

            fn generate_timestamp_sequence(&self, seconds: u64, subsec_nanos: u32) -> (Self::Output, u64, u32) {
                let unix_nanos = self.adjust.apply(parts_to_nanos(seconds, subsec_nanos));

                let mut counter;
                let (mut timestamp, should_reseed) = self.timestamp.get().advance(unix_nanos);

                if should_reseed {
                    // The time has moved forwards: regenerate the counter.
                    counter = Counter::reseed(&self.precision, &timestamp);
                } else {
                    // The time hasn't moved forwards: increment the counter.
                    counter = self.counter.get().increment(&self.precision, &timestamp);

                    // If the counter has overflowed its 42 bits, increment the timestamp by a
                    // millisecond and reseed the counter.
                    if counter.has_overflowed() {
                        timestamp = timestamp.increment();
                        counter = Counter::reseed(&self.precision, &timestamp);
                    }
                };

                self.timestamp.set(timestamp);
                self.counter.set(counter);

                let (seconds, subsec_nanos) = timestamp.to_parts();

                (counter.value, seconds, subsec_nanos)
            }

            fn usable_bits(&self) -> usize {
                USABLE_BITS
            }
        }

        #[derive(Debug, Default, Clone, Copy)]
        struct ReseedingTimestamp {
            last_seed: u64,
            unix_nanos: u128,
        }

        impl ReseedingTimestamp {
            fn saturating_from_unix_nanos(unix_nanos: u128) -> Self {
                const MAX_UNIX_NANOS: u128 = (u64::MAX as u128) * 1_000_000 + 999_999;

                let unix_nanos = cmp::min(unix_nanos, MAX_UNIX_NANOS);

                // Reseed when the millisecond advances.
                let last_seed = (unix_nanos / 1_000_000) as u64;

                ReseedingTimestamp { last_seed, unix_nanos }
            }

            fn to_parts(self) -> (u64, u32) {
                (
                    (self.unix_nanos / 1_000_000_000) as u64,
                    (self.unix_nanos % 1_000_000_000) as u32,
                )
            }

            fn advance(self, unix_nanos: u128) -> (Self, bool) {
                let incoming = ReseedingTimestamp::saturating_from_unix_nanos(unix_nanos);

                (
                    ReseedingTimestamp::saturating_from_unix_nanos(cmp::max(self.unix_nanos, unix_nanos)),
                    incoming.last_seed > self.last_seed,
                )
            }

            fn increment(self) -> Self {
                ReseedingTimestamp::saturating_from_unix_nanos(Adjust::by_millis(1).apply(self.unix_nanos))
            }
        }

        #[derive(Debug, Default, Clone, Copy)]
        struct Counter {
            value: u64,
        }

        impl Counter {
            fn reseed(precision: &Precision, timestamp: &ReseedingTimestamp) -> Self {
                Counter {
                    value: precision.apply(crate::rng::u64() & RESEED_MASK, timestamp),
                }
            }

            fn increment(self, precision: &Precision, timestamp: &ReseedingTimestamp) -> Self {
                // Never overflows a u64.
                let next = self.value + 1;

                Counter {
                    value: cmp::max(next, precision.apply(next, timestamp)),
                }
            }

            fn has_overflowed(self) -> bool {
                self.value > MAX_COUNTER
            }
        }

        #[derive(Debug, Default, Clone, Copy)]
        struct Adjust {
            by_ns: u128,
        }

        impl Adjust {
            fn by_millis(millis: u32) -> Self {
                Adjust {
                    by_ns: (millis as u128).saturating_mul(1_000_000),
                }
            }

            fn apply(&self, nanos: u128) -> u128 {
                nanos.saturating_add(self.by_ns)
            }
        }

        #[derive(Debug, Default, Clone, Copy)]
        struct Precision {
            bits: usize,
            factor: u64,
            mask: u64,
            shift: u64,
        }

        impl Precision {
            fn new(bits: usize) -> Self {
                // The mask and shift paste the sub-millisecond precision into the most significant
                // bits of the counter.
                let mask = u64::MAX >> (64 - USABLE_BITS + bits);
                let shift = (USABLE_BITS - bits) as u64;

                // The factor scales the sub-millisecond precision down to the number of bits.
                let factor = (999_999 / u64::pow(2, bits as u32)) + 1;

                Precision {
                    bits,
                    factor,
                    mask,
                    shift,
                }
            }

            fn apply(&self, counter: u64, timestamp: &ReseedingTimestamp) -> u64 {
                if self.bits == 0 {
                    return counter;
                }

                let additional = (timestamp.unix_nanos % 1_000_000) as u64 / self.factor;

                (counter & self.mask) | (additional << self.shift)
            }
        }
    }
}
