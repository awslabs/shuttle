//! This module implements a simple serialization scheme for schedules (`Schedule`) that tries to
//! produce small printable strings. This is useful for roundtripping schedules in test outputs.

use crate::runtime::task::TaskId;
use crate::scheduler::{Schedule, ScheduleStep};
use bitvec::prelude::*;
use varmint::*;

// The serialization format is this:
//   [task id bitwidth] [number of schedule steps] [seed] [step]*
// The bitwidth, number of steps, and seed are encoded as VarInts, so are at least one byte.
// The steps are densely packed bitstrings. The leading bit of a step is 0 if it's a task ID or 1
// if it's a random value. If it's a task ID, the following `bitwidth` bits are the task ID. If it's
// a random value, there are no following bits.
//
// We encode the binary serialization as a hex string for easy copy/pasting.

const SCHEDULE_MAGIC_V2: u8 = 0x91;

pub(crate) fn serialize_schedule(schedule: &Schedule) -> String {
    let &max_task_id = schedule
        .steps
        .iter()
        .filter_map(|s| match s {
            ScheduleStep::Task(tid) => Some(tid),
            _ => None,
        })
        .max()
        .unwrap_or(&TaskId::from(0));
    let task_id_bits = std::mem::size_of_val(&max_task_id) * 8 - usize::from(max_task_id).leading_zeros() as usize;
    let task_id_bits = task_id_bits.max(1);

    let mut encoded = bitvec![Lsb0, u8; 0; schedule.steps.len() * (1 + task_id_bits)];
    let mut offset = 0usize;
    for step in &schedule.steps {
        match step {
            ScheduleStep::Task(tid) => {
                encoded.set(offset, false);
                encoded[offset + 1..offset + 1 + task_id_bits].store(usize::from(*tid));
                offset += 1 + task_id_bits;
            }
            ScheduleStep::Random => {
                encoded.set(offset, true);
                offset += 1;
            }
        }
    }

    let mut buf = Vec::with_capacity(
        1 + len_usize_varint(task_id_bits)
            + len_usize_varint(schedule.len())
            + len_u64_varint(schedule.seed)
            + encoded.len(),
    );
    buf.push(SCHEDULE_MAGIC_V2);
    buf.write_usize_varint(task_id_bits).unwrap();
    buf.write_usize_varint(schedule.len()).unwrap();
    buf.write_u64_varint(schedule.seed).unwrap();
    buf.extend(encoded.as_raw_slice());
    hex::encode(buf)
}

pub(crate) fn deserialize_schedule(str: &str) -> Option<Schedule> {
    let bytes = hex::decode(str).ok()?;

    let version = bytes[0];
    if version != SCHEDULE_MAGIC_V2 {
        return None;
    }
    let mut bytes = &bytes[1..];

    let task_id_bits = bytes.read_usize_varint().ok()?;
    let schedule_len = bytes.read_usize_varint().ok()?;
    let seed = bytes.read_u64_varint().ok()?;

    let encoded = BitSlice::<Lsb0, _>::from_slice(bytes).unwrap();
    let mut offset = 0usize;
    let mut steps = Vec::with_capacity(schedule_len);
    while steps.len() < schedule_len {
        if *encoded.get(offset).unwrap() {
            steps.push(ScheduleStep::Random);
            offset += 1;
        } else {
            let tid = encoded[offset + 1..offset + 1 + task_id_bits].load::<usize>();
            steps.push(ScheduleStep::Task(TaskId::from(tid)));
            offset += 1 + task_id_bits;
        }
    }

    Some(Schedule { seed, steps })
}

#[cfg(test)]
mod test {
    use super::*;
    use proptest::{collection::vec, prelude::*};

    // Schedules of up to 100 steps with TaskIds up to 10000
    fn schedule_strategy() -> impl Strategy<Value = Schedule> {
        let step_strategy = prop_oneof![
            Just(ScheduleStep::Random),
            (0usize..10000).prop_map(|tid| ScheduleStep::Task(TaskId::from(tid)))
        ];
        let steps_strategy = vec(step_strategy, (0, 100));
        (any::<u64>(), steps_strategy).prop_map(|(seed, steps)| Schedule { seed, steps })
    }

    fn check_roundtrip(schedule: Schedule) {
        let encoded = serialize_schedule(&schedule);
        let decoded = deserialize_schedule(encoded.as_str()).unwrap();
        assert_eq!(schedule, decoded);
    }

    #[test]
    fn serialization_roundtrip_basic() {
        check_roundtrip(Schedule {
            seed: 10,
            steps: vec![ScheduleStep::Random],
        });
        check_roundtrip(Schedule {
            seed: 10,
            steps: vec![ScheduleStep::Task(TaskId::from(0))],
        });
    }

    proptest! {
        #[test]
        fn serialization_roundtrip_proptest(schedule in schedule_strategy()) {
            check_roundtrip(schedule);
        }
    }
}
