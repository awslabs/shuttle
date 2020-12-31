//! This module implements a simple serialization scheme for schedules (`Schedule`) that tries to
//! produce small printable strings. This is useful for roundtripping schedules in test outputs.

use crate::runtime::task::TaskId;
use crate::scheduler::Schedule;
use bitvec::prelude::*;
use varmint::*;

// The serialization format is this:
//   [task id bitwidth] [number of schedule steps] [task id]*
// The bitwidth and number of steps are encoded as VarInts, so are at least one byte.
// The task IDs are densely packed bitstrings, where each task ID is encoded as exactly `bitwidth`
// bits. Since our schedules will often have very few distinct tasks, this encoding lets us create
// short schedule strings.
//
// We encode the binary serialization as a hex string for easy copy/pasting.

const SCHEDULE_MAGIC_V1: u8 = 0x34;

pub(crate) fn serialize_schedule(schedule: &Schedule) -> String {
    let &max_task_id = schedule.iter().max().unwrap_or(&TaskId::from(0));
    let task_id_bits = std::mem::size_of_val(&max_task_id) * 8 - usize::from(max_task_id).leading_zeros() as usize;

    let mut encoded = bitvec![Lsb0, u8; 0; schedule.len() * task_id_bits];
    if task_id_bits > 0 {
        for (&tid, target_bits) in schedule.iter().zip(encoded[..].chunks_exact_mut(task_id_bits)) {
            target_bits.store(usize::from(tid));
        }
    }

    let mut buf =
        Vec::with_capacity(1 + len_usize_varint(task_id_bits) + len_usize_varint(schedule.len()) + encoded.len());
    buf.push(SCHEDULE_MAGIC_V1);
    buf.write_usize_varint(task_id_bits).unwrap();
    buf.write_usize_varint(schedule.len()).unwrap();
    buf.extend(encoded.as_slice());
    hex::encode(buf)
}

pub(crate) fn deserialize_schedule(str: &str) -> Option<Schedule> {
    let bytes = hex::decode(str).ok()?;

    let version = bytes[0];
    if version != SCHEDULE_MAGIC_V1 {
        return None;
    }
    let mut bytes = &bytes[1..];

    let task_id_bits = bytes.read_usize_varint().ok()?;
    let schedule_len = bytes.read_usize_varint().ok()?;

    if schedule_len == 0 {
        return Some(vec![].into());
    } else if task_id_bits == 0 {
        return Some(vec![0; schedule_len].into());
    }

    let encoded = BitSlice::<Lsb0, _>::from_slice(bytes).unwrap();

    let mut schedule = Vec::with_capacity(bytes.len() * 8 / task_id_bits);
    for tid_bits in encoded.chunks_exact(task_id_bits).take(schedule_len) {
        let tid = tid_bits.load::<usize>();
        schedule.push(tid);
    }
    Some(schedule.into())
}

#[cfg(test)]
mod test {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_pcg::Pcg64Mcg;

    #[test]
    fn roundtrip() {
        let mut rng = Pcg64Mcg::seed_from_u64(0x12345678);
        for _ in 0..1000 {
            let max_task_id = 1 + rng.gen::<usize>() % 200;
            let len = rng.gen::<usize>() % 2000;
            let mut schedule = Vec::with_capacity(len);
            for _ in 0..len {
                schedule.push(TaskId::from(rng.gen::<usize>() % max_task_id));
            }
            let schedule = Schedule(schedule);

            let encoded = serialize_schedule(&schedule);
            let decoded = deserialize_schedule(encoded.as_str()).unwrap();
            assert_eq!(*schedule, *decoded);
        }
    }
}
