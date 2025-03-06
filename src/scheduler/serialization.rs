//! This module implements a simple serialization scheme for schedules (`Schedule`) that tries to
//! produce small printable strings. This is useful for roundtripping schedules in test outputs.

use crate::runtime::task::TaskId;
use crate::scheduler::{Schedule, ScheduleStep};
use bitvec::prelude::*;

/// A simplified version of the deprecated [`varmint`](https://github.com/mycorrhiza/varmint-rs)
/// crate, used under the MIT license.
mod varint {
    pub fn space_needed(val: u64) -> usize {
        let used_bits = u64::MIN.leading_zeros() - val.leading_zeros();
        std::cmp::max((used_bits + 6) as usize / 7, 1)
    }

    pub trait WriteVarInt {
        fn write_u64_varint(&mut self, val: u64) -> std::io::Result<()>;
    }

    impl<R: std::io::Write> WriteVarInt for R {
        fn write_u64_varint(&mut self, mut val: u64) -> std::io::Result<()> {
            loop {
                let current = (val & 0x7F) as u8;
                val >>= 7;
                if val == 0 {
                    self.write_all(&[current])?;
                    return Ok(());
                } else {
                    self.write_all(&[current | 0x80])?;
                }
            }
        }
    }

    pub trait ReadVarInt {
        fn read_u64_varint(&mut self) -> std::io::Result<u64>;
    }

    fn read_u8<R: std::io::Read>(reader: &mut R) -> std::io::Result<u8> {
        let mut buffer = [0u8];
        reader.read_exact(&mut buffer)?;
        Ok(buffer[0])
    }

    impl<R: std::io::Read> ReadVarInt for R {
        fn read_u64_varint(&mut self) -> std::io::Result<u64> {
            let first = read_u8(self)?;
            if first & 0x80 == 0 {
                return Ok(u64::from(first));
            }

            let mut result = u64::from(first & 0x7F);
            let mut offset = 7;

            loop {
                let current = read_u8(self)?;
                result += u64::from(current & 0x7F) << offset;
                if current & 0x80 == 0 {
                    return Ok(result);
                }
                offset += 7;
                if offset == 63 {
                    let last = read_u8(self)?;
                    if last == 0x01 {
                        return Ok(result + (1 << offset));
                    } else {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "varint exceeded 64 bits long",
                        ));
                    }
                }
            }
        }
    }
}

// The serialization format is this:
//   [task id bitwidth] [number of schedule steps] [seed] [step]*
// The bitwidth, number of steps, and seed are encoded as VarInts, so are at least one byte.
// The steps are densely packed bitstrings. The leading bit of a step is 0 if it's a task ID or 1
// if it's a random value. If it's a task ID, the following `bitwidth` bits are the task ID. If it's
// a random value, there are no following bits.
//
// We encode the binary serialization as a hex string for easy copy/pasting.

const SCHEDULE_MAGIC_V2: u8 = 0x91;

const LINE_WIDTH: usize = 76;

pub(crate) fn serialize_schedule(schedule: &Schedule) -> String {
    use self::varint::{WriteVarInt, space_needed};

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

    let mut encoded = bitvec![u8, Lsb0; 0; schedule.steps.len() * (1 + task_id_bits)];
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
        1 + space_needed(task_id_bits as u64)
            + space_needed(schedule.len() as u64)
            + space_needed(schedule.seed)
            + encoded.len(),
    );
    buf.push(SCHEDULE_MAGIC_V2);
    buf.write_u64_varint(task_id_bits as u64).unwrap();
    buf.write_u64_varint(schedule.len() as u64).unwrap();
    buf.write_u64_varint(schedule.seed).unwrap();
    buf.extend(encoded.as_raw_slice());

    let serialized = hex::encode(buf);
    let lines = serialized.as_bytes().chunks(LINE_WIDTH).collect::<Vec<_>>();
    let wrapped = lines.join(&[b'\n'][..]);
    String::from_utf8(wrapped).unwrap()
}

pub(crate) fn deserialize_schedule(str: &str) -> Option<Schedule> {
    use self::varint::ReadVarInt;

    let str: String = str.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = hex::decode(str).ok()?;

    let version = bytes[0];
    if version != SCHEDULE_MAGIC_V2 {
        return None;
    }
    let mut bytes = &bytes[1..];

    let task_id_bits = bytes.read_u64_varint().ok()? as usize;
    let schedule_len = bytes.read_u64_varint().ok()? as usize;
    let seed = bytes.read_u64_varint().ok()?;

    let encoded = BitSlice::<_, Lsb0>::from_slice(bytes);
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
