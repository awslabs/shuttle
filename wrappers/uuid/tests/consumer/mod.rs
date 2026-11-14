//! Code written against uuid, as a crate that depends on `shuttle-uuid` under the name `uuid` would
//! have it. The same source is compiled against `shuttle-uuid` without the `shuttle` feature, where
//! it is uuid, and against `shuttle-uuid-inner`, which is what `shuttle-uuid` is under `shuttle`.
//! The test that includes it names the crate `uuid` and provides `real_crate`, which stands in for a
//! crate built on uuid itself.
//!
//! The checks hold whichever crate `uuid` is, so they assume nothing about the actual time.

use std::collections::{BTreeSet, HashSet};
use std::str::FromStr;
use uuid::fmt::{Braced, Hyphenated, Simple, Urn};
use uuid::{Builder, ContextV1, ContextV7, NoContext, NonNilUuid, Timestamp, Uuid, Variant, Version};

const PARSED_AT_COMPILE_TIME: Uuid = uuid::uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8");

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Record {
    id: Uuid,
    #[serde(with = "uuid::serde::compact")]
    compact: Uuid,
    #[serde(with = "uuid::serde::simple::option")]
    parent: Option<Uuid>,
    non_nil: NonNilUuid,
    braced: Braced,
}

/// Exercises uuid's API.
pub fn exercise() {
    parse_and_format();
    bytes_and_fields();
    random_and_time_based();
    timestamps_and_contexts();
    builder_and_non_nil();
    serde_round_trip();
    collections();
    boundary();
}

fn parse_and_format() {
    let text = "67e55044-10b1-426f-9247-bb680e5fe0c8";
    let uuid = Uuid::parse_str(text).unwrap();
    assert_eq!(uuid, PARSED_AT_COMPILE_TIME);
    assert_eq!(Uuid::from_str(text).unwrap(), uuid);
    assert_eq!(Uuid::try_from(text).unwrap(), uuid);
    assert_eq!(Uuid::try_from(text.to_string()).unwrap(), uuid);
    assert_eq!(Uuid::try_parse(text), Ok(uuid));
    assert_eq!(Uuid::try_parse_ascii(text.as_bytes()), Ok(uuid));
    assert!(Uuid::parse_str("not a uuid").is_err());

    assert_eq!(uuid.to_string(), text);
    assert_eq!(format!("{uuid:?}"), text);
    assert_eq!(format!("{uuid:X}"), text.to_uppercase());
    assert_eq!(String::from(uuid), text);
    assert_eq!(uuid.hyphenated().to_string(), text);
    assert_eq!(uuid.simple().to_string(), "67e5504410b1426f9247bb680e5fe0c8");
    assert_eq!(uuid.urn().to_string(), format!("urn:uuid:{text}"));
    assert_eq!(uuid.braced().to_string(), format!("{{{text}}}"));
    assert_eq!(format!("{:?}", uuid.hyphenated()), format!("Hyphenated({text})"));

    let mut buffer = Uuid::encode_buffer();
    assert_eq!(
        uuid.simple().encode_upper(&mut buffer),
        "67E5504410B1426F9247BB680E5FE0C8"
    );
    assert_eq!(uuid.as_hyphenated().encode_lower(&mut buffer).len(), Hyphenated::LENGTH);
    assert_eq!(*uuid.as_urn().as_uuid(), uuid);
    assert_eq!(
        Simple::from_str("67e5504410b1426f9247bb680e5fe0c8")
            .unwrap()
            .into_uuid(),
        uuid
    );
    assert_eq!(Urn::from_uuid(uuid).to_string().len(), Urn::LENGTH);
    assert_eq!(Uuid::from(uuid.braced()), uuid);
}

fn bytes_and_fields() {
    let uuid = PARSED_AT_COMPILE_TIME;
    assert_eq!(Uuid::from_bytes(*uuid.as_bytes()), uuid);
    assert_eq!(Uuid::from_bytes(uuid.into_bytes()), uuid);
    assert_eq!(Uuid::from_bytes_le(uuid.to_bytes_le()), uuid);
    assert_eq!(*Uuid::from_bytes_ref(uuid.as_bytes()), uuid);
    assert_eq!(Uuid::from_slice(uuid.as_bytes()).unwrap(), uuid);
    assert_eq!(Uuid::from_u128(uuid.as_u128()), uuid);
    assert_eq!(Uuid::from_u128_le(uuid.to_u128_le()), uuid);
    let (high, low) = uuid.as_u64_pair();
    assert_eq!(Uuid::from_u64_pair(high, low), uuid);
    let (d1, d2, d3, d4) = uuid.as_fields();
    assert_eq!(Uuid::from_fields(d1, d2, d3, d4), uuid);
    let (d1, d2, d3, d4) = uuid.to_fields_le();
    assert_eq!(Uuid::from_fields_le(d1, d2, d3, d4), uuid);
    assert_eq!(Vec::from(uuid), uuid.as_bytes().to_vec());
    assert_eq!(Uuid::try_from(uuid.as_bytes().to_vec()).unwrap(), uuid);
    assert_eq!(AsRef::<[u8]>::as_ref(&uuid), uuid.as_bytes());

    assert!(Uuid::nil().is_nil() && Uuid::default().is_nil());
    assert!(Uuid::max().is_max());
    assert_eq!(uuid.get_version(), Some(Version::Random));
    assert_eq!(uuid.get_variant(), Variant::RFC4122);
    assert_eq!(uuid.get_timestamp(), None);
    assert_eq!(uuid.get_node_id(), None);
}

fn random_and_time_based() {
    let random = Uuid::new_v4();
    assert_eq!(random.get_version(), Some(Version::Random));
    assert_eq!(random.get_variant(), Variant::RFC4122);
    assert_ne!(random, Uuid::new_v4());

    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    assert_eq!(first.get_version(), Some(Version::SortRand));
    assert!(first < second, "version 7 UUIDs are ordered by creation");

    let node_id = [1, 2, 3, 4, 5, 6];
    let v1 = Uuid::now_v1(&node_id);
    assert_eq!(v1.get_version(), Some(Version::Mac));
    assert_eq!(v1.get_node_id(), Some(node_id));
    let v6 = Uuid::now_v6(&node_id);
    assert_eq!(v6.get_version(), Some(Version::SortMac));

    // The name-based and custom versions are deterministic, and match uuid's own test fixtures.
    let v3 = Uuid::new_v3(&Uuid::NAMESPACE_DNS, b"example.org");
    assert_eq!(v3.to_string(), "04738bdf-b25a-3829-a801-b21a1d25095b");
    let v5 = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"example.org");
    assert_eq!(v5.to_string(), "aad03681-8b63-5304-89e0-8ca8f49461b5");
    assert_eq!(Uuid::new_v8([0xff; 16]).get_version(), Some(Version::Custom));
}

fn timestamps_and_contexts() {
    let ts = Timestamp::from_unix(NoContext, 1_497_624_119, 1_234);
    assert_eq!(ts.to_unix(), (1_497_624_119, 1_234));
    let v7 = Uuid::new_v7(ts);
    assert!(v7.hyphenated().to_string().starts_with("015cb15a-86d8-7"));
    let (seconds, nanos) = v7.get_timestamp().unwrap().to_unix();
    assert_eq!((seconds, nanos / 1_000_000), (1_497_624_119, 0));

    let context = ContextV1::new(42);
    let ts = Timestamp::from_unix(&context, 1_497_624_119, 1_234);
    let node_id = [1, 2, 3, 4, 5, 6];
    let v1 = Uuid::new_v1(ts, &node_id);
    assert_eq!(v1.get_timestamp().unwrap().to_gregorian(), ts.to_gregorian());
    assert_eq!(ts.to_gregorian().1, 42);
    let v6 = Uuid::new_v6(Timestamp::from_unix(&context, 1_497_624_119, 1_234), &node_id);
    assert_eq!(v6.get_timestamp().unwrap().to_gregorian().1, 43);

    // A `ContextV7` orders the UUIDs it makes within the same millisecond.
    let context = ContextV7::new().with_additional_precision();
    let first = Uuid::new_v7(Timestamp::from_unix(&context, 1_497_624_119, 1_234));
    let second = Uuid::new_v7(Timestamp::from_unix(&context, 1_497_624_119, 1_234));
    assert!(first < second);

    let _random: ContextV1 = ContextV1::new_random();
    let now = Timestamp::now(NoContext);
    assert!(Uuid::new_v7(now).get_timestamp().is_some());
    let system_time = std::time::SystemTime::from(now);
    assert_eq!(Timestamp::try_from(system_time).unwrap().to_unix(), now.to_unix());
}

fn builder_and_non_nil() {
    let bytes = *PARSED_AT_COMPILE_TIME.as_bytes();
    let random = Builder::from_random_bytes(bytes).into_uuid();
    assert_eq!(random.get_version(), Some(Version::Random));
    let mut builder = Builder::from_bytes(bytes);
    builder.set_variant(Variant::Microsoft).set_version(Version::Sha1);
    assert_eq!(builder.as_uuid().get_variant(), Variant::Microsoft);
    assert_eq!(builder.into_uuid().get_version(), Some(Version::Sha1));
    assert_eq!(
        Builder::nil().with_version(Version::Md5).into_uuid().get_version_num(),
        3
    );

    let non_nil = NonNilUuid::new(PARSED_AT_COMPILE_TIME).unwrap();
    assert_eq!(non_nil.get(), PARSED_AT_COMPILE_TIME);
    assert_eq!(non_nil, PARSED_AT_COMPILE_TIME);
    assert!(NonNilUuid::new(Uuid::nil()).is_none());
    assert!(NonNilUuid::try_from(Uuid::nil()).is_err());
    assert_eq!(Uuid::from(non_nil), PARSED_AT_COMPILE_TIME);
    assert_ne!(NonNilUuid::new_v4(), NonNilUuid::new_v4());
    assert_eq!(NonNilUuid::now_v7().get().get_version(), Some(Version::SortRand));
    assert_eq!(non_nil.to_string(), PARSED_AT_COMPILE_TIME.to_string());
    assert_eq!(size_of::<Option<NonNilUuid>>(), size_of::<Uuid>());
}

fn serde_round_trip() {
    let record = Record {
        id: Uuid::new_v4(),
        compact: PARSED_AT_COMPILE_TIME,
        parent: Some(Uuid::now_v7()),
        non_nil: NonNilUuid::new_v4(),
        braced: PARSED_AT_COMPILE_TIME.braced(),
    };
    let json = serde_json::to_string(&record).unwrap();
    assert!(json.contains("\"{67e55044-10b1-426f-9247-bb680e5fe0c8}\""), "{json}");
    assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), record);
}

fn collections() {
    let uuids = [Uuid::new_v4(), Uuid::new_v4(), PARSED_AT_COMPILE_TIME, Uuid::nil()];
    assert_eq!(uuids.iter().collect::<HashSet<_>>().len(), uuids.len());
    let sorted = uuids.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(sorted.first(), Some(&Uuid::nil()));
}

// Without `shuttle`, `Uuid` is uuid's own, and each `.into()` here converts a type to itself.
#[allow(clippy::useless_conversion)]
fn boundary() {
    let uuid = Uuid::new_v4();
    let back: Uuid = crate::real_crate::echo(uuid.into()).into();
    assert_eq!(back, uuid);
    let record = crate::real_crate::Record { id: uuid.into() };
    assert_eq!(Uuid::from(record.id), uuid);
    let non_nil = NonNilUuid::new_v4();
    assert_eq!(
        NonNilUuid::from(crate::real_crate::echo_non_nil(non_nil.into())),
        non_nil
    );
}
