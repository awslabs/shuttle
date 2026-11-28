//! The constructors for each version of UUID. The ones that read randomness or the clock use
//! random bits from Shuttle (see `crate::rng`) and the fake clock; the rest delegate to uuid's.

#[cfg(any(feature = "v1", feature = "v4", feature = "v6", feature = "v7", feature = "v8"))]
use crate::Builder;
#[cfg(any(feature = "v1", feature = "v6", feature = "v7"))]
use crate::Timestamp;
use crate::Uuid;

impl Uuid {
    /// Create a new version 1 UUID using the fake clock and node ID.
    ///
    /// # Panics
    ///
    /// Panics outside of a Shuttle test.
    #[cfg(all(feature = "v1", feature = "std", feature = "rng"))]
    pub fn now_v1(node_id: &[u8; 6]) -> Self {
        Self::new_v1(crate::clock::now_v1(), node_id)
    }

    /// Create a new version 1 UUID using the given timestamp and node ID.
    #[cfg(feature = "v1")]
    pub fn new_v1(ts: Timestamp, node_id: &[u8; 6]) -> Self {
        let (ticks, counter) = ts.to_gregorian();

        Builder::from_gregorian_timestamp(ticks, counter, node_id).into_uuid()
    }

    /// Creates a UUID using a name from a namespace, based on the MD5 hash.
    #[cfg(feature = "v3")]
    pub fn new_v3(namespace: &Uuid, name: &[u8]) -> Uuid {
        Uuid(uuid_orig::Uuid::new_v3(&namespace.0, name))
    }

    /// Creates a random UUID, whose random bits come from Shuttle: from its random number generator,
    /// or from the execution's own generator where the scheduler provides no random data (see the
    /// crate documentation).
    ///
    /// # Panics
    ///
    /// Panics outside of a Shuttle test.
    #[cfg(feature = "v4")]
    pub fn new_v4() -> Uuid {
        Builder::from_random_bytes(crate::rng::bytes()).into_uuid()
    }

    /// Creates a UUID using a name from a namespace, based on the SHA-1 hash.
    #[cfg(feature = "v5")]
    pub fn new_v5(namespace: &Uuid, name: &[u8]) -> Uuid {
        Uuid(uuid_orig::Uuid::new_v5(&namespace.0, name))
    }

    /// Create a new version 6 UUID using the fake clock and node ID.
    ///
    /// # Panics
    ///
    /// Panics outside of a Shuttle test.
    #[cfg(all(feature = "v6", feature = "std", feature = "rng"))]
    pub fn now_v6(node_id: &[u8; 6]) -> Self {
        Self::new_v6(crate::clock::now_v1(), node_id)
    }

    /// Create a new version 6 UUID using the given timestamp and a node ID.
    #[cfg(feature = "v6")]
    pub fn new_v6(ts: Timestamp, node_id: &[u8; 6]) -> Self {
        let (ticks, counter) = ts.to_gregorian();

        Builder::from_sorted_gregorian_timestamp(ticks, counter, node_id).into_uuid()
    }

    /// Create a new version 7 UUID using the fake clock. All UUIDs that this function creates in an
    /// execution are ordered by their creation.
    ///
    /// # Panics
    ///
    /// Panics outside of a Shuttle test.
    #[cfg(all(feature = "v7", feature = "std"))]
    pub fn now_v7() -> Self {
        Self::new_v7(crate::clock::now_v7())
    }

    /// Create a new version 7 UUID using a time value and random bytes from Shuttle (see the crate
    /// documentation).
    ///
    /// # Panics
    ///
    /// Panics outside of a Shuttle test.
    #[cfg(feature = "v7")]
    pub fn new_v7(ts: Timestamp) -> Self {
        // As in uuid 1.29.2, with random bits from Shuttle.
        let (secs, nanos) = ts.to_unix();
        let millis = secs.saturating_mul(1000).saturating_add(nanos as u64 / 1_000_000);

        let (mut counter, counter_bits) = ts.counter();

        // `Builder::from_unix_timestamp_millis` takes the top 80 bits of this value, so the counter
        // is placed directly below the version nibble and shifted around the variant.
        const RAND_A_BITS: u32 = 12;
        const PAYLOAD_BITS: u32 = RAND_A_BITS + 62;

        // Retain the most significant bits of a counter wider than the payload.
        let mut counter_bits = core::cmp::min(counter_bits as u32, 128);
        if counter_bits > PAYLOAD_BITS {
            counter >>= counter_bits - PAYLOAD_BITS;
            counter_bits = PAYLOAD_BITS;
        }

        // Shift the counter around the variant field.
        if counter_bits > RAND_A_BITS {
            let mask = u128::MAX << (counter_bits - RAND_A_BITS);
            counter = (counter & !mask) | ((counter & mask) << 2);
            counter_bits += 2;
        }

        let counter_and_random = if counter_bits == 0 {
            crate::rng::u128()
        } else {
            let shift = 124 - counter_bits;

            (crate::rng::u128() & (u128::MAX >> (128 - shift))) | (counter << shift)
        };

        let bytes = counter_and_random.to_be_bytes();
        let mut counter_random_bytes = [0; 10];
        counter_random_bytes.copy_from_slice(&bytes[..10]);
        Builder::from_unix_timestamp_millis(millis, &counter_random_bytes).into_uuid()
    }

    /// Creates a custom UUID comprised almost entirely of user-supplied bytes.
    #[cfg(feature = "v8")]
    pub const fn new_v8(buf: [u8; 16]) -> Uuid {
        Builder::from_custom_bytes(buf).into_uuid()
    }
}
