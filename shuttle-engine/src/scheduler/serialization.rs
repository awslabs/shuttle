//! This module implements a simple serialization scheme for schedules (`Schedule`) that tries to
//! produce small printable strings. This is useful for roundtripping schedules in test outputs.

use crate::config::{ScheduleEncoding, ScheduleTextEncoding};
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
                        return Err(std::io::Error::other("varint exceeded 64 bits long"));
                    }
                }
            }
        }
    }
}

// Every serialized schedule begins with a magic byte identifying its encoding, so that
// `deserialize_schedule` can read any format we have ever emitted. See `ScheduleEncoding` for how
// to choose the format used when writing.
//
// V2 (`ScheduleEncoding::FixedWidth`) is:
//   [magic] [task id bitwidth] [number of schedule steps] [seed] [step]*
// The bitwidth, number of steps, and seed are encoded as VarInts, so are at least one byte.
// The steps are densely packed bitstrings. The leading bit of a step is 0 if it's a task ID or 1
// if it's a random value. If it's a task ID, the following `bitwidth` bits are the task ID. If it's
// a random value, there are no following bits.
//
// V3 (`ScheduleEncoding::MoveToFront`) is described in the `mtf` module below.
//
// Those bytes are then rendered as text for easy copy/pasting, either as hex or using the dense
// Unicode alphabet in the `unicode_text` module below. Which one was used is detected on read, so
// both are always accepted.

const SCHEDULE_MAGIC_V2: u8 = 0x91;
const SCHEDULE_MAGIC_V3: u8 = 0x92;

/// Width at which a serialized schedule is wrapped. Deserialization strips all whitespace, so this
/// only affects readability. 120 is a common modern terminal and source-file width; the old value of
/// 76 came from email conventions and cost a third more lines than necessary.
const LINE_WIDTH: usize = 120;

/// Serialize a schedule using the default encodings ([`ScheduleEncoding::MoveToFront`] rendered with
/// [`ScheduleTextEncoding::Unicode`]).
pub fn serialize_schedule(schedule: &Schedule) -> String {
    serialize_schedule_with(schedule, ScheduleEncoding::default(), ScheduleTextEncoding::default())
}

/// Serialize a schedule using the given encoding, rendered with the given alphabet.
pub fn serialize_schedule_with(
    schedule: &Schedule,
    encoding: ScheduleEncoding,
    text_encoding: ScheduleTextEncoding,
) -> String {
    let buf = match encoding {
        ScheduleEncoding::FixedWidth => serialize_fixed_width(schedule),
        ScheduleEncoding::MoveToFront => mtf::serialize(schedule),
    };
    match text_encoding {
        ScheduleTextEncoding::Hex => wrap_lines(hex::encode(buf).chars()),
        ScheduleTextEncoding::Unicode { marks_per_cell } => unicode_text::encode(&buf, marks_per_cell),
    }
}

/// Wrap at [`LINE_WIDTH`] characters. Deserialization strips whitespace, so this is cosmetic.
fn wrap_lines(chars: impl Iterator<Item = char>) -> String {
    let mut wrapped = String::new();
    for (i, c) in chars.enumerate() {
        if i > 0 && i.is_multiple_of(LINE_WIDTH) {
            wrapped.push('\n');
        }
        wrapped.push(c);
    }
    wrapped
}

fn serialize_fixed_width(schedule: &Schedule) -> Vec<u8> {
    use self::varint::{space_needed, WriteVarInt};

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

    buf
}

/// Deserialize a schedule produced by [`serialize_schedule`] or [`serialize_schedule_with`]. The
/// encoding is detected automatically, so any format Shuttle has ever emitted can be replayed.
pub fn deserialize_schedule(str: &str) -> Option<Schedule> {
    let str: String = str.chars().filter(|c| !c.is_whitespace()).collect();

    // The Unicode alphabet deliberately contains no ASCII, so the presence of any non-ASCII
    // character unambiguously identifies which alphabet was used.
    let bytes = if str.is_ascii() {
        hex::decode(str).ok()?
    } else {
        unicode_text::decode(&str)?
    };

    match *bytes.first()? {
        SCHEDULE_MAGIC_V2 => deserialize_fixed_width(&bytes[1..]),
        SCHEDULE_MAGIC_V3 => mtf::deserialize(&bytes[1..]),
        _ => None,
    }
}

fn deserialize_fixed_width(mut bytes: &[u8]) -> Option<Schedule> {
    use self::varint::ReadVarInt;

    let task_id_bits = usize::try_from(bytes.read_u64_varint().ok()?).ok()?;
    let schedule_len = usize::try_from(bytes.read_u64_varint().ok()?).ok()?;
    let seed = bytes.read_u64_varint().ok()?;

    if task_id_bits > usize::BITS as usize {
        return None;
    }

    let encoded = BitSlice::<_, Lsb0>::from_slice(bytes);
    // Every step occupies at least one bit, so a length claiming more steps than there are bits is
    // corrupt. Checking up front means we never reserve a bogus amount of memory.
    if schedule_len > encoded.len() {
        return None;
    }

    let mut offset = 0usize;
    let mut steps = Vec::with_capacity(schedule_len);
    while steps.len() < schedule_len {
        if *encoded.get(offset)? {
            steps.push(ScheduleStep::Random);
            offset += 1;
        } else {
            let end = offset.checked_add(1)?.checked_add(task_id_bits)?;
            let tid = encoded.get(offset + 1..end)?.load::<usize>();
            steps.push(ScheduleStep::Task(TaskId::from(tid)));
            offset = end;
        }
    }

    Some(Schedule { seed, steps })
}

/// A dense Unicode alphabet for rendering a serialized schedule as printable text.
///
/// Hex spends one character on four bits. This module spends one *terminal column* on 14 bits plus 8
/// bits for each combining mark stacked onto it, because combining marks are zero-width: they render
/// on top of the base character rather than beside it. The default depth is `u32::MAX`, which is more
/// marks than any schedule has bits, so the whole schedule lands in a single cell and prints on one
/// line.
///
/// The layout of a cell is one base character followed by up to `marks_per_cell` marks. Bits are
/// consumed most-significant-first and laid down in that order, so a decoder that simply walks the
/// characters in order recovers the same bit stream. That means the stacking depth does not need to
/// be recorded: it is not stored anywhere, any depth decodes, and re-wrapped or re-flowed text still
/// decodes.
///
/// The alphabet is chosen so that the text survives a round trip through a terminal, a clipboard and
/// an editor:
///
/// * Base characters are single-column (East Asian width `N`, `Na` or `H`), so one cell is one
///   column. Wide characters carry more bits per character but no more bits per column, and
///   ambiguous-width ones become two columns in a terminal configured for East Asian text.
/// * Base characters are left-to-right or bidi-neutral. Right-to-left ones would reorder on display.
/// * Marks are variation selectors. They are `Default_Ignorable_Code_Point`, which is what makes them
///   render as nothing, and they have a canonical combining class of zero. The combining class
///   matters twice over. Unicode normalization *sorts* marks by class, so a pool mixing classes would
///   have its order silently rearranged by any tool that normalizes. And class zero makes them
///   *starters* rather than non-starters, which exempts them from the 30-non-starter cap in the
///   Stream-Safe Text Format of UAX #15; a pool of non-starters would have `U+034F COMBINING GRAPHEME
///   JOINER` injected into deep stacks, which UAX #15 notes is not canonically equivalent to the
///   original. Stacks thousands deep are byte-identical under NFC, NFD, NFKC and NFKD.
/// * Nothing in either set has a decomposition or changes under any of the four normalization forms,
///   and no base character composes with any mark.
/// * Neither set contains ASCII, which is what lets `deserialize_schedule` tell this alphabet from
///   hex, or whitespace, which deserialization strips.
///
/// The one hazard that cannot be designed away is that terminals cap how many combining marks they
/// will attach to a single cell, and either drop the excess or render it as separate cells. Dropping
/// corrupts the schedule; a checksum over the payload turns that from a schedule that replays
/// incorrectly into an error. Rendering it as separate cells is merely ugly: the data survives, but
/// the output occupies one column per character instead of one per cell. Use a shallower
/// `marks_per_cell`, or `ScheduleTextEncoding::Hex`, if that matters more than density.
mod unicode_text {
    use super::{varint, LINE_WIDTH};
    use bitvec::prelude::*;

    /// Bits carried by a base character.
    ///
    /// 2^14 is the ceiling: only 23,544 code points satisfy every constraint above, so 2^15 would
    /// require either unassigned code points, which forfeits Unicode's guarantee that already
    /// normalized text stays normalized in future versions and so would stop old schedules from
    /// replaying, or ambiguous-width ones, which would cost more columns than the extra bit buys.
    pub(super) const BASE_BITS: usize = 14;
    /// Bits carried by a combining mark.
    ///
    /// 2^8 is the ceiling here too, and by a wider margin: there are only 263 code points in the
    /// whole of Unicode that are `Default_Ignorable_Code_Point`, category `Mn`, and combining class
    /// zero. The pool below is essentially all of them.
    const MARK_BITS: usize = 8;

    /// Single-column, left-to-right or neutral, normalization-stable, non-ASCII, assigned code
    /// points. Exactly `2^BASE_BITS` of them. Generated and validated against Unicode 16.0.0; see
    /// `base_alphabet_is_well_formed` for the invariants that are checked at test time.
    #[rustfmt::skip]
    pub(super) const BASE_RANGES: [(u32, u32); 87] = [
        (0x0262, 0x02AF), //   78  LATIN LETTER SMALL
        (0x048A, 0x04C0), //   55  CYRILLIC CAPITAL LETTER
        (0x04FA, 0x052F), //   54  CYRILLIC CAPITAL LETTER
        (0x0559, 0x0586), //   46  ARMENIAN MODIFIER LETTER
        (0x0E01, 0x0E30), //   48  THAI CHARACTER KO
        (0x1160, 0x1248), //  233  HANGUL JUNGSEONG FILLER
        (0x12D8, 0x1310), //   57  ETHIOPIC SYLLABLE ZA
        (0x1318, 0x135A), //   67  ETHIOPIC SYLLABLE GGA
        (0x13A0, 0x13F5), //   86  CHEROKEE LETTER A
        (0x1400, 0x167F), //  640  CANADIAN SYLLABICS HYPHEN
        (0x16A0, 0x16F8), //   89  RUNIC LETTER FEHU
        (0x1780, 0x17B3), //   52  KHMER LETTER KA
        (0x1820, 0x1878), //   89  MONGOLIAN LETTER A
        (0x18B0, 0x18F5), //   70  CANADIAN SYLLABICS OY
        (0x19DE, 0x1A16), //   57  NEW TAI LUE
        (0x1A1E, 0x1A54), //   55  BUGINESE PALLAWA
        (0x1BAE, 0x1BE5), //   56  SUNDANESE LETTER KHA
        (0x1C4D, 0x1C8A), //   62  LEPCHA LETTER TTA
        (0x232B, 0x23E8), //  190  ERASE TO THE
        (0x23F4, 0x2429), //   54  BLACK MEDIUM LEFT-POINTING
        (0x27C0, 0x2A0B), //  588  THREE DIMENSIONAL ANGLE
        (0x2A0D, 0x2A73), //  103  FINITE PART INTEGRAL
        (0x2A77, 0x2ADB), //  101  EQUALS SIGN WITH
        (0x2ADD, 0x2B1A), //   62  NONFORKING
        (0x2B1D, 0x2B4F), //   51  BLACK VERY SMALL
        (0x2B97, 0x2C7B), //  229  SYMBOL FOR TYPE
        (0x2C7E, 0x2CEE), //  113  LATIN CAPITAL LETTER
        (0x2CF9, 0x2CFD), //    5  COPTIC OLD NUBIAN
        (0x2D30, 0x2D67), //   56  TIFINAGH LETTER YA
        (0x2E00, 0x2E5D), //   94  RIGHT ANGLE SUBSTITUTION
        (0xA4D0, 0xA62B), //  348  LISU LETTER BA
        (0xA640, 0xA66E), //   47  CYRILLIC CAPITAL LETTER
        (0xA6A0, 0xA6EF), //   80  BAMUM LETTER A
        (0xA700, 0xA76F), //  112  MODIFIER LETTER CHINESE
        (0xA771, 0xA7CD), //   93  LATIN SMALL LETTER
        (0xA840, 0xA877), //   56  PHAGS-PA LETTER KA
        (0xA882, 0xA8B3), //   50  SAURASHTRA LETTER A
        (0xA984, 0xA9B2), //   47  JAVANESE LETTER A
        (0xAA7E, 0xAAAF), //   50  MYANMAR LETTER SHWE
        (0xAB70, 0xABE2), //  115  CHEROKEE SMALL LETTER
        (0xD7CB, 0xD7FB), //   49  HANGUL JONGSEONG NIEUN-RIEUL
        (0x10080, 0x100FA), //  123  LINEAR B IDEOGRAM
        (0x10137, 0x1018E), //   88  AEGEAN WEIGHT BASE
        (0x102A0, 0x102D0), //   49  CARIAN LETTER A
        (0x10400, 0x1049D), //  158  DESERET CAPITAL LETTER
        (0x10530, 0x10563), //   52  CAUCASIAN ALBANIAN LETTER
        (0x10600, 0x10736), //  311  LINEAR A SIGN
        (0x11003, 0x11037), //   53  BRAHMI SIGN JIHVAMULIYA
        (0x11183, 0x111B2), //   48  SHARADA LETTER A
        (0x112B0, 0x112DE), //   47  KHUDAWADI LETTER A
        (0x11400, 0x11434), //   53  NEWA LETTER A
        (0x11480, 0x114AF), //   48  TIRHUTA ANJI
        (0x11580, 0x115AE), //   47  SIDDHAM LETTER A
        (0x11600, 0x1162F), //   48  MODI LETTER A
        (0x118A0, 0x118F2), //   83  WARANG CITI CAPITAL
        (0x11A5C, 0x11A89), //   46  SOYOMBO LETTER KA
        (0x11AB0, 0x11AF8), //   73  CANADIAN SYLLABICS NATTILIK
        (0x11FC0, 0x11FF1), //   50  TAMIL FRACTION ONE
        (0x11FFF, 0x12399), //  923  TAMIL PUNCTUATION END
        (0x12400, 0x1246E), //  111  CUNEIFORM NUMERIC SIGN
        (0x12480, 0x12543), //  196  CUNEIFORM SIGN AB
        (0x12F90, 0x12FF2), //   99  CYPRO-MINOAN SIGN CM001
        (0x13000, 0x1342F), // 1072  EGYPTIAN HIEROGLYPH A001
        (0x13460, 0x143FA), // 3995  EGYPTIAN HIEROGLYPH-13460
        (0x14400, 0x14646), //  583  ANATOLIAN HIEROGLYPH A001
        (0x16800, 0x16A38), //  569  BAMUM LETTER PHASE-A
        (0x16A6E, 0x16ABE), //   81  MRO DANDA
        (0x16B00, 0x16B2F), //   48  PAHAWH HMONG VOWEL
        (0x16E40, 0x16E9A), //   91  MEDEFAIDRIN CAPITAL LETTER
        (0x16F00, 0x16F4A), //   75  MIAO LETTER PA
        (0x1BC00, 0x1BC6A), //  107  DUPLOYAN LETTER H
        (0x1CC00, 0x1CCD5), //  214  UP-POINTING GO-KART
        (0x1CD00, 0x1CEB3), //  436  BLOCK OCTANT-3
        (0x1CF50, 0x1CFC3), //  116  ZNAMENNY NEUME KRYUK
        (0x1D000, 0x1D0F5), //  246  BYZANTINE MUSICAL SYMBOL
        (0x1D129, 0x1D15D), //   53  MUSICAL SYMBOL MULTIPLE
        (0x1D200, 0x1D241), //   66  GREEK VOCAL NOTATION
        (0x1D800, 0x1D9FF), //  512  SIGNWRITING HAND-FIST INDEX
        (0x1F030, 0x1F093), //  100  DOMINO TILE HORIZONTAL
        (0x1F5A5, 0x1F5FA), //   86  DESKTOP COMPUTER
        (0x1F650, 0x1F67F), //   48  NORTH WEST POINTING
        (0x1F700, 0x1F776), //  119  ALCHEMICAL SYMBOL FOR
        (0x1F77B, 0x1F7D9), //   95  HAUMEA
        (0x1F810, 0x1F847), //   56  LEFTWARDS ARROW WITH
        (0x1FA00, 0x1FA53), //   84  NEUTRAL CHESS KING
        (0x1FB00, 0x1FB92), //  147  BLOCK SEXTANT-1
        (0x1FB94, 0x1FBEF), //   92  LEFT HALF INVERSE
    ];

    /// Zero-width, combining-class-zero, inert marks. Exactly `2^MARK_BITS` of them.
    const MARK_RANGES: [(u32, u32); 2] = [
        (0xFE00, 0xFE0F),   //    16  Variation Selectors
        (0xE0100, 0xE01EF), //   240  Variation Selectors Supplement
    ];

    /// CRC-32 (IEEE), used to detect a schedule that lost characters in transit.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    fn to_char(ranges: &[(u32, u32)], mut value: u32) -> char {
        for &(lo, hi) in ranges {
            let len = hi - lo + 1;
            if value < len {
                return char::from_u32(lo + value).expect("alphabet contains only valid scalars");
            }
            value -= len;
        }
        unreachable!("value out of range for alphabet")
    }

    fn to_value(ranges: &[(u32, u32)], c: char) -> Option<u32> {
        let cp = u32::from(c);
        let mut base = 0;
        for &(lo, hi) in ranges {
            if (lo..=hi).contains(&cp) {
                return Some(base + cp - lo);
            }
            base += hi - lo + 1;
        }
        None
    }

    /// Wrap the payload in a self-delimiting, checksummed container.
    fn frame(payload: &[u8]) -> Vec<u8> {
        use varint::WriteVarInt;

        let mut framed = Vec::with_capacity(payload.len() + 14);
        framed.write_u64_varint(payload.len() as u64).unwrap();
        framed.extend_from_slice(payload);
        framed.extend_from_slice(&crc32(payload).to_le_bytes());
        framed
    }

    fn unframe(framed: &[u8]) -> Option<Vec<u8>> {
        use varint::ReadVarInt;

        let mut cursor = framed;
        let len = usize::try_from(cursor.read_u64_varint().ok()?).ok()?;
        let payload = cursor.get(..len)?.to_vec();
        let checksum = u32::from_le_bytes(cursor.get(len..len + 4)?.try_into().unwrap());
        (checksum == crc32(&payload)).then_some(payload)
    }

    pub(super) fn encode(payload: &[u8], marks_per_cell: u32) -> String {
        let framed = frame(payload);
        let bits = BitSlice::<u8, Msb0>::from_slice(&framed);

        let mut out = String::new();
        let mut pos = 0usize;
        let mut cell = 0usize;
        // A zero-length payload cannot occur (every encoding starts with a magic byte), but the loop
        // below would emit nothing for one, which `decode` would reject rather than mis-read.
        while pos < bits.len() {
            if cell > 0 && cell.is_multiple_of(LINE_WIDTH) {
                out.push('\n');
            }
            out.push(to_char(&BASE_RANGES, take(bits, &mut pos, BASE_BITS)));
            for _ in 0..marks_per_cell {
                if pos >= bits.len() {
                    break;
                }
                out.push(to_char(&MARK_RANGES, take(bits, &mut pos, MARK_BITS)));
            }
            cell += 1;
        }
        out
    }

    /// Consume up to `width` bits, most significant first. A short final read is padded with zeros in
    /// the low bits so that the bits which *were* present keep their positions.
    fn take(bits: &BitSlice<u8, Msb0>, pos: &mut usize, width: usize) -> u32 {
        let end = (*pos + width).min(bits.len());
        let mut value = 0u32;
        for i in *pos..end {
            value = (value << 1) | u32::from(bits[i]);
        }
        value <<= width - (end - *pos);
        *pos = end;
        value
    }

    pub(super) fn decode(str: &str) -> Option<Vec<u8>> {
        let mut bits: BitVec<u8, Msb0> = BitVec::new();
        let mut seen_base = false;

        for c in str.chars() {
            let (value, width) = match to_value(&MARK_RANGES, c) {
                // A mark before any base character means the text lost its leading cell.
                Some(value) if seen_base => (value, MARK_BITS),
                Some(_) => return None,
                None => {
                    seen_base = true;
                    (to_value(&BASE_RANGES, c)?, BASE_BITS)
                }
            };
            for i in (0..width).rev() {
                bits.push((value >> i) & 1 == 1);
            }
        }

        unframe(&bits.into_vec())
    }
}

/// The V3 schedule encoding, which codes each step by its rank in a move-to-front list of the most
/// recently scheduled tasks.
///
/// The motivation is that the V2 encoding sizes its task ID field from the largest task ID anywhere
/// in the schedule, so a test that spawns many tasks pays for that width on every single step, even
/// though only a handful of tasks are typically live at any moment. Ranking against a move-to-front
/// list replaces "which of the N tasks in this test" with "which of the few recently run tasks",
/// which is a much smaller number.
///
/// The layout is:
///   [magic] [number of schedule steps: varint] [seed: 8 bytes little-endian] [step]*
///
/// The seed is stored fixed-width because seeds are uniformly random `u64`s, for which a varint
/// costs 10 bytes rather than 8.
///
/// Each step is a single code word `v`, interpreted against the move-to-front list as it stands at
/// that point in the schedule (so the decoder, which rebuilds the list as it goes, always agrees
/// with the encoder):
///   * `v == 0`: a `ScheduleStep::Random`. Random steps do not disturb the list.
///   * `1 ..= len`: a `ScheduleStep::Task` for the task at rank `v - 1`, which then moves to
///     the front of the list.
///   * `len + 1`: a task being scheduled for the first time, followed by its `TaskId` as an Elias
///     delta code. It is then inserted at the front of the list.
///
/// Code words themselves use a flat 4-bit field, with the all-ones value escaping to an Elias delta
/// code for the remainder. Ranks are empirically close to uniform over the live task set rather than
/// power-law distributed, which is why a flat field beats coding the rank directly with a
/// variable-length code.
mod mtf {
    use super::{varint, Schedule, ScheduleStep, TaskId, SCHEDULE_MAGIC_V3};
    use bitvec::prelude::*;

    /// Width of the flat code word field.
    const CODE_BITS: usize = 4;
    /// Code word value that escapes to an Elias delta code.
    const ESCAPE: u64 = (1 << CODE_BITS) - 1;

    struct BitWriter {
        bits: BitVec<u8, Lsb0>,
    }

    impl BitWriter {
        fn new(capacity_hint: usize) -> Self {
            Self {
                bits: BitVec::with_capacity(capacity_hint),
            }
        }

        /// Write the low `width` bits of `val`, most significant bit first.
        fn write_bits(&mut self, val: u64, width: usize) {
            for i in (0..width).rev() {
                self.bits.push((val >> i) & 1 == 1);
            }
        }

        /// Write `n >= 1` as an Elias delta code.
        fn write_elias_delta(&mut self, n: u64) {
            debug_assert!(n >= 1);
            // `l` is floor(log2(n)), i.e. the number of bits of `n` after its leading one.
            let l = (u64::BITS - 1 - n.leading_zeros()) as usize;
            // Elias gamma of `l + 1`, whose leading one doubles as the terminator of the zero run.
            let m = l as u64 + 1;
            let m_width = (u64::BITS - m.leading_zeros()) as usize;
            self.write_bits(0, m_width - 1);
            self.write_bits(m, m_width);
            // The remaining bits of `n` below its leading one.
            self.write_bits(n, l);
        }

        /// Write a code word using the flat field, escaping to an Elias delta code if it does not
        /// fit.
        fn write_code(&mut self, v: u64) {
            if v < ESCAPE {
                self.write_bits(v, CODE_BITS);
            } else {
                self.write_bits(ESCAPE, CODE_BITS);
                self.write_elias_delta(v - ESCAPE + 1);
            }
        }
    }

    struct BitReader<'a> {
        bits: &'a BitSlice<u8, Lsb0>,
        pos: usize,
    }

    impl<'a> BitReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                bits: BitSlice::from_slice(bytes),
                pos: 0,
            }
        }

        fn read_bit(&mut self) -> Option<bool> {
            let bit = *self.bits.get(self.pos)?;
            self.pos += 1;
            Some(bit)
        }

        fn read_bits(&mut self, width: usize) -> Option<u64> {
            debug_assert!(width <= 64);
            let mut val = 0u64;
            for _ in 0..width {
                val = (val << 1) | u64::from(self.read_bit()?);
            }
            Some(val)
        }

        fn read_elias_delta(&mut self) -> Option<u64> {
            let mut zeros = 0usize;
            while !self.read_bit()? {
                zeros += 1;
                if zeros >= u64::BITS as usize {
                    return None;
                }
            }
            let m = (1u64 << zeros) | self.read_bits(zeros)?;
            let l = usize::try_from(m.checked_sub(1)?).ok()?;
            if l >= u64::BITS as usize {
                return None;
            }
            Some((1u64 << l) | self.read_bits(l)?)
        }

        fn read_code(&mut self) -> Option<u64> {
            let v = self.read_bits(CODE_BITS)?;
            if v < ESCAPE {
                Some(v)
            } else {
                Some(self.read_elias_delta()? + ESCAPE - 1)
            }
        }
    }

    /// Find `task` in the move-to-front list and move it to the front, returning its rank before
    /// the move. Returns `None` if the task is not in the list yet, in which case it is appended at
    /// the front.
    fn promote(list: &mut Vec<usize>, task: usize) -> Option<usize> {
        match list.iter().position(|t| *t == task) {
            Some(rank) => {
                // Rotating the prefix is O(rank) rather than the O(len) of a remove + insert, which
                // matters because ranks are small but schedules can be very long.
                list[..=rank].rotate_right(1);
                Some(rank)
            }
            None => {
                list.push(task);
                list.rotate_right(1);
                None
            }
        }
    }

    pub(super) fn serialize(schedule: &Schedule) -> Vec<u8> {
        use varint::WriteVarInt;

        let mut writer = BitWriter::new(schedule.steps.len() * (CODE_BITS + 1));
        let mut list: Vec<usize> = Vec::new();

        for step in &schedule.steps {
            match step {
                ScheduleStep::Random => writer.write_code(0),
                ScheduleStep::Task(tid) => {
                    let tid = usize::from(*tid);
                    // Read the length before promoting, since promoting a new task grows the list.
                    let len = list.len() as u64;
                    match promote(&mut list, tid) {
                        Some(rank) => writer.write_code(rank as u64 + 1),
                        None => {
                            writer.write_code(len + 1);
                            writer.write_elias_delta(tid as u64 + 1);
                        }
                    }
                }
            }
        }

        let mut buf = Vec::with_capacity(3 + 8 + writer.bits.len() / 8);
        buf.push(SCHEDULE_MAGIC_V3);
        buf.write_u64_varint(schedule.len() as u64).unwrap();
        buf.extend_from_slice(&schedule.seed.to_le_bytes());
        buf.extend(writer.bits.as_raw_slice());

        buf
    }

    pub(super) fn deserialize(mut bytes: &[u8]) -> Option<Schedule> {
        use varint::ReadVarInt;

        let schedule_len = usize::try_from(bytes.read_u64_varint().ok()?).ok()?;
        let seed = u64::from_le_bytes(bytes.get(..8)?.try_into().unwrap());

        let mut reader = BitReader::new(&bytes[8..]);
        // Every step occupies at least one code word, so a length claiming more steps than that is
        // corrupt. Checking up front means we never reserve a bogus amount of memory.
        if schedule_len > reader.bits.len() / CODE_BITS {
            return None;
        }

        let mut list: Vec<usize> = Vec::new();
        let mut steps = Vec::with_capacity(schedule_len);

        while steps.len() < schedule_len {
            let v = reader.read_code()?;
            if v == 0 {
                steps.push(ScheduleStep::Random);
                continue;
            }
            let rank = (v - 1) as usize;
            let tid = if rank < list.len() {
                let tid = list[rank];
                list[..=rank].rotate_right(1);
                tid
            } else if rank == list.len() {
                let tid = usize::try_from(reader.read_elias_delta()?.checked_sub(1)?).ok()?;
                // A repeat of an existing task must be coded by its rank, so seeing one here means
                // the input is corrupt and would desynchronize the list.
                if promote(&mut list, tid).is_some() {
                    return None;
                }
                tid
            } else {
                return None;
            };
            steps.push(ScheduleStep::Task(TaskId::from(tid)));
        }

        Some(Schedule { seed, steps })
    }
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

    const ENCODINGS: [ScheduleEncoding; 2] = [ScheduleEncoding::FixedWidth, ScheduleEncoding::MoveToFront];

    const TEXT_ENCODINGS: [ScheduleTextEncoding; 7] = [
        ScheduleTextEncoding::Hex,
        ScheduleTextEncoding::Unicode { marks_per_cell: 0 },
        ScheduleTextEncoding::Unicode { marks_per_cell: 1 },
        ScheduleTextEncoding::Unicode { marks_per_cell: 4 },
        ScheduleTextEncoding::Unicode { marks_per_cell: 255 },
        ScheduleTextEncoding::Unicode { marks_per_cell: 4096 },
        ScheduleTextEncoding::Unicode {
            marks_per_cell: u32::MAX,
        },
    ];

    /// Every combination of payload encoding and alphabet must reproduce the schedule exactly.
    fn check_roundtrip(schedule: Schedule) {
        for encoding in ENCODINGS {
            for text in TEXT_ENCODINGS {
                let encoded = serialize_schedule_with(&schedule, encoding, text);
                let decoded = deserialize_schedule(encoded.as_str())
                    .unwrap_or_else(|| panic!("{encoding:?}/{text:?} failed to decode {encoded:?}"));
                assert_eq!(schedule, decoded, "{encoding:?}/{text:?} roundtrip mismatch");
            }
        }
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

    #[test]
    fn serialization_roundtrip_empty() {
        check_roundtrip(Schedule { seed: 0, steps: vec![] });
    }

    #[test]
    fn serialization_roundtrip_extreme_values() {
        check_roundtrip(Schedule {
            seed: u64::MAX,
            steps: vec![
                ScheduleStep::Task(TaskId::from(usize::MAX - 1)),
                ScheduleStep::Random,
                ScheduleStep::Task(TaskId::from(usize::MAX - 1)),
                ScheduleStep::Task(TaskId::from(0)),
            ],
        });
    }

    /// The defaults are what unadorned `serialize_schedule` emits.
    #[test]
    fn defaults_are_move_to_front_and_unicode() {
        assert_eq!(ScheduleEncoding::default(), ScheduleEncoding::MoveToFront);
        assert_eq!(
            ScheduleTextEncoding::default(),
            ScheduleTextEncoding::Unicode {
                marks_per_cell: u32::MAX
            }
        );

        let schedule = Schedule {
            seed: 10,
            steps: vec![ScheduleStep::Task(TaskId::from(7)), ScheduleStep::Random],
        };
        assert_eq!(
            serialize_schedule(&schedule),
            serialize_schedule_with(
                &schedule,
                ScheduleEncoding::MoveToFront,
                ScheduleTextEncoding::Unicode {
                    marks_per_cell: u32::MAX
                }
            )
        );
        assert_eq!(deserialize_schedule(&serialize_schedule(&schedule)), Some(schedule));
    }

    /// The two axes are independent: the alphabet must not care which payload encoding it carries,
    /// and vice versa.
    #[test]
    fn text_encoding_is_independent_of_payload_encoding() {
        let schedule = Schedule {
            seed: 42,
            steps: (0..300).map(|i| ScheduleStep::Task(TaskId::from(i % 9))).collect(),
        };
        for encoding in ENCODINGS {
            let via_hex =
                deserialize_schedule(&serialize_schedule_with(&schedule, encoding, ScheduleTextEncoding::Hex));
            let via_unicode = deserialize_schedule(&serialize_schedule_with(
                &schedule,
                encoding,
                ScheduleTextEncoding::Unicode { marks_per_cell: 4 },
            ));
            assert_eq!(via_hex.as_ref(), Some(&schedule), "{encoding:?} via hex");
            assert_eq!(via_unicode.as_ref(), Some(&schedule), "{encoding:?} via unicode");
        }
    }

    /// The Unicode alphabet exists to cut printed columns. Since its marks are zero-width, columns
    /// are counted as characters that are not marks.
    #[test]
    fn unicode_alphabet_reduces_columns() {
        let schedule = Schedule {
            seed: 42,
            steps: (0..4000).map(|i| ScheduleStep::Task(TaskId::from(i % 9))).collect(),
        };
        let payload_bits = serialize_schedule_with(&schedule, ScheduleEncoding::MoveToFront, ScheduleTextEncoding::Hex)
            .chars()
            .filter(|c| !c.is_whitespace())
            .count()
            * 4;

        let columns = |text: &str| text.chars().filter(|c| !c.is_whitespace() && !is_mark(*c)).count();

        let hex = serialize_schedule_with(&schedule, ScheduleEncoding::MoveToFront, ScheduleTextEncoding::Hex);
        let hex_columns = columns(&hex);

        let mut previous = hex_columns;
        for marks in [0u32, 1, 4, 16, 64, 255, 4096, u32::MAX] {
            let text = serialize_schedule_with(
                &schedule,
                ScheduleEncoding::MoveToFront,
                ScheduleTextEncoding::Unicode { marks_per_cell: marks },
            );
            let cols = columns(&text);
            // Deeper stacking never costs columns. It stops helping once the whole payload fits in
            // one cell, which is why this is not a strict inequality.
            assert!(
                cols <= previous,
                "{marks} marks/cell: {cols} columns, more than {previous}"
            );

            // Each column carries 14 bits plus 8 per mark, so the column count should sit just above
            // the information-theoretic minimum. The container adds a length and a checksum, hence
            // the small allowance. Computed in u64 so that `u32::MAX` marks cannot overflow.
            let per_column = 14 + 8 * u64::from(marks);
            let floor = payload_bits as u64 / per_column;
            assert!(
                cols as u64 >= floor,
                "{marks} marks/cell: {cols} columns beats the {floor} column floor"
            );
            assert!(
                (cols as u64) < floor + floor / 10 + 8,
                "{marks} marks/cell: {cols} columns is well above {floor}"
            );

            previous = cols;
        }

        // Base characters alone, with no reliance on marks rendering as zero-width, already beat hex
        // by better than three to one.
        let flat = serialize_schedule_with(
            &schedule,
            ScheduleEncoding::MoveToFront,
            ScheduleTextEncoding::Unicode { marks_per_cell: 0 },
        );
        assert!(
            columns(&flat) * 3 < hex_columns,
            "{} columns vs hex's {hex_columns}",
            columns(&flat)
        );

        // And the default packs the whole schedule into a single cell, so it prints on one line.
        let dense = serialize_schedule_with(
            &schedule,
            ScheduleEncoding::MoveToFront,
            ScheduleTextEncoding::default(),
        );
        assert_eq!(columns(&dense), 1, "the default should emit exactly one column");
        assert_eq!(dense.lines().count(), 1, "the default should emit exactly one line");
    }

    /// The base alphabet is generated, so guard the invariants the generator enforced. The Unicode
    /// properties themselves (width, bidi class, normalization stability) cannot be rechecked without
    /// a UCD dependency, but everything structural can be.
    #[test]
    fn base_alphabet_is_well_formed() {
        // `to_char` indexes the ranges by a `BASE_BITS`-wide value, so the count has to match exactly
        // or high values would panic on the `unreachable!`.
        let total: usize = unicode_text::BASE_RANGES
            .iter()
            .map(|(lo, hi)| (hi - lo + 1) as usize)
            .sum();
        assert_eq!(total, 1 << unicode_text::BASE_BITS, "base alphabet is the wrong size");

        let mut previous_end = 0u32;
        for &(lo, hi) in &unicode_text::BASE_RANGES {
            assert!(lo <= hi, "range {lo:#X}..={hi:#X} is inverted");
            assert!(lo > previous_end, "range at {lo:#X} is out of order or overlaps");
            assert!(lo >= 0x80, "range at {lo:#X} contains ASCII");
            previous_end = hi;

            for cp in [lo, hi] {
                let c = char::from_u32(cp).unwrap_or_else(|| panic!("{cp:#X} is not a scalar value"));
                assert!(!c.is_whitespace(), "{cp:#X} is whitespace, which decoding strips");
                assert!(!is_mark(c), "{cp:#X} is in both the base and mark alphabets");
            }
        }
    }

    fn is_mark(c: char) -> bool {
        matches!(u32::from(c), 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
    }

    /// A schedule that loses characters in transit, which is what happens if a terminal drops
    /// combining marks, must be reported rather than silently replayed as a different schedule.
    #[test]
    fn damaged_unicode_is_detected() {
        let schedule = Schedule {
            seed: 42,
            steps: (0..500).map(|i| ScheduleStep::Task(TaskId::from(i % 9))).collect(),
        };
        let encoded = serialize_schedule_with(
            &schedule,
            ScheduleEncoding::MoveToFront,
            ScheduleTextEncoding::Unicode { marks_per_cell: 4 },
        );
        let chars = encoded.chars().filter(|c| !c.is_whitespace()).collect::<Vec<_>>();
        assert_eq!(deserialize_schedule(&encoded).as_ref(), Some(&schedule));

        // Drop each mark in turn, simulating a terminal that clipped the stack.
        let mut checked = 0;
        for (i, c) in chars.iter().enumerate() {
            if !is_mark(*c) {
                continue;
            }
            let damaged: String = chars[..i].iter().chain(&chars[i + 1..]).collect();
            assert_ne!(
                deserialize_schedule(&damaged).as_ref(),
                Some(&schedule),
                "dropping the mark at {i} went undetected"
            );
            checked += 1;
            if checked >= 100 {
                break;
            }
        }
        assert!(checked > 0, "no marks were emitted to damage");

        // Dropping a whole cell, or truncating, must also be caught.
        for cut in [1usize, 2, 7, chars.len() / 2] {
            let damaged: String = chars[..chars.len() - cut].iter().collect();
            assert_ne!(deserialize_schedule(&damaged).as_ref(), Some(&schedule));
        }
    }

    /// Marks are meaningless without a preceding base character, so leading marks are corruption.
    #[test]
    fn leading_mark_is_rejected() {
        assert_eq!(deserialize_schedule("\u{FE00}\u{1400}"), None);
        assert_eq!(deserialize_schedule("\u{E0100}"), None);
    }

    /// Characters outside the alphabet are not silently ignored.
    #[test]
    fn unknown_characters_are_rejected() {
        let schedule = Schedule {
            seed: 1,
            steps: vec![ScheduleStep::Random],
        };
        let encoded = serialize_schedule(&schedule);
        assert_eq!(deserialize_schedule(&encoded).as_ref(), Some(&schedule));
        assert_eq!(
            deserialize_schedule(&format!("{encoded}\u{4E00}")),
            None,
            "CJK is not in the alphabet"
        );
        assert_eq!(
            deserialize_schedule(&format!("{encoded}q")),
            None,
            "mixed ASCII and unicode"
        );
    }

    /// Schedules serialized by older versions of Shuttle must keep replaying, so the V2 encoding has
    /// to stay byte-for-byte stable. These strings are taken from Shuttle's own test suite.
    #[test]
    fn fixed_width_encoding_is_stable() {
        for encoded in [
            "9102110090205124480000",
            "910102fe93a9cef4f3faaf5a04",
            "91022ceac7d5bcb1a7fcc5d801a8050ea528954032492693491200000000",
            "910216c5ebeace8f90be89c601804082124090024901",
            "910228b4d9dee0deaddaee970100440aa64d93a44dc9b62d8914254a",
        ] {
            let schedule = deserialize_schedule(encoded).expect("V2 schedule should still decode");
            assert_eq!(
                serialize_schedule_with(&schedule, ScheduleEncoding::FixedWidth, ScheduleTextEncoding::Hex),
                encoded,
                "V2 encoding is no longer byte-stable"
            );
        }
    }

    /// The whole point of the MTF encoding: a schedule that rotates among a few tasks should not pay
    /// for the largest task ID in the test on every step.
    #[test]
    fn move_to_front_is_smaller_for_many_tasks() {
        // 2000 steps rotating among 4 tasks, but with one high task ID present to widen the
        // fixed-width field.
        let mut steps = vec![ScheduleStep::Task(TaskId::from(5000))];
        for i in 0..2000 {
            steps.push(ScheduleStep::Task(TaskId::from(i % 4)));
        }
        let schedule = Schedule { seed: 12345, steps };

        let len = |encoding| {
            serialize_schedule_with(&schedule, encoding, ScheduleTextEncoding::Hex)
                .chars()
                .filter(|c| !c.is_whitespace())
                .count()
        };
        let fixed = len(ScheduleEncoding::FixedWidth);
        let mtf = len(ScheduleEncoding::MoveToFront);
        assert!(
            mtf * 2 < fixed,
            "expected MTF ({mtf}) to be much smaller than fixed ({fixed})"
        );
    }

    /// Long schedules are wrapped for readability, and that wrapping has to survive a round trip.
    #[test]
    fn long_schedules_are_wrapped() {
        let schedule = Schedule {
            seed: 7,
            steps: (0..5000).map(|i| ScheduleStep::Task(TaskId::from(i % 6))).collect(),
        };

        for encoding in ENCODINGS {
            for text in TEXT_ENCODINGS {
                let encoded = serialize_schedule_with(&schedule, encoding, text);
                let lines = encoded.lines().collect::<Vec<_>>();
                // Very deep mark stacking fits this whole schedule inside one line, which is the
                // point of it; only require wrapping when there is more than a line's worth.
                let columns = encoded.chars().filter(|c| !c.is_whitespace() && !is_mark(*c)).count();
                assert_eq!(
                    lines.len(),
                    columns.div_ceil(LINE_WIDTH),
                    "{encoding:?}/{text:?} wrapped {columns} columns into {} lines",
                    lines.len()
                );
                for (i, line) in lines.iter().enumerate() {
                    // Marks are zero-width, so a line's length is its non-mark characters.
                    let width = line.chars().filter(|c| !is_mark(*c)).count();
                    assert!(width <= LINE_WIDTH, "{encoding:?}/{text:?} line {i} is {width} wide");
                    // Only the last line may be short.
                    if i + 1 < lines.len() {
                        assert_eq!(width, LINE_WIDTH, "{encoding:?}/{text:?} line {i} is short");
                    }
                }
                assert_eq!(deserialize_schedule(&encoded).as_ref(), Some(&schedule));
            }
        }
    }

    /// Whitespace is insignificant, so schedules wrapped at any width still replay. This matters
    /// because schedules saved before the wrap width changed are wrapped at the old width, and
    /// because hand-pasted schedules pick up arbitrary reflowing.
    #[test]
    fn wrapping_is_insignificant() {
        let schedule = Schedule {
            seed: 7,
            steps: (0..400).map(|i| ScheduleStep::Task(TaskId::from(i % 6))).collect(),
        };
        let encoded = serialize_schedule(&schedule);
        let flat = encoded.replace('\n', "");

        for width in [1, 2, 3, 76, 119, 120, 121, flat.len()] {
            let rewrapped = flat
                .chars()
                .enumerate()
                .flat_map(|(i, c)| {
                    let brk = (i > 0 && i.is_multiple_of(width)).then_some('\n');
                    brk.into_iter().chain(std::iter::once(c))
                })
                .collect::<String>();
            assert_eq!(
                deserialize_schedule(&rewrapped).as_ref(),
                Some(&schedule),
                "failed when wrapped at {width}"
            );
        }
        assert_eq!(deserialize_schedule(&format!("  {flat}\n\n")).as_ref(), Some(&schedule));
    }

    /// Truncated or corrupt input should be rejected rather than panicking, since these strings are
    /// pasted in by hand.
    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(deserialize_schedule(""), None);
        assert_eq!(deserialize_schedule("00"), None, "unknown magic byte");
        assert_eq!(deserialize_schedule("zz"), None, "not hex");
        assert_eq!(deserialize_schedule("9"), None, "odd number of hex digits");

        let schedule = Schedule {
            seed: 99,
            steps: (0..50).map(|i| ScheduleStep::Task(TaskId::from(i))).collect(),
        };
        // Every proper prefix of a valid MTF schedule is either invalid or decodes to something
        // shorter, but must never panic.
        let encoded = serialize_schedule_with(&schedule, ScheduleEncoding::MoveToFront, ScheduleTextEncoding::Hex);
        for len in (2..encoded.len()).step_by(2) {
            let _ = deserialize_schedule(&encoded[..len]);
        }

        // Same for the Unicode alphabet, truncated at every character boundary.
        let encoded = serialize_schedule(&schedule);
        let chars = encoded.chars().collect::<Vec<_>>();
        for len in 0..chars.len() {
            let _ = deserialize_schedule(&chars[..len].iter().collect::<String>());
        }
    }

    /// Arbitrary sequences drawn from the Unicode alphabet must be rejected or decoded, never
    /// panic. This covers the alphabet lookup and the framing, which arbitrary *bytes* cannot reach.
    #[test]
    fn arbitrary_unicode_does_not_panic() {
        let alphabet: Vec<char> = [0x1400u32, 0x1401, 0x167F, 0x27C0, 0x143FA, 0x16986, 0xFE00, 0xE01EF]
            .iter()
            .map(|cp| char::from_u32(*cp).unwrap())
            .collect();

        // Deterministic pseudo-random walk over the alphabet.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for len in 0..300 {
            let s: String = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    alphabet[(state % alphabet.len() as u64) as usize]
                })
                .collect();
            let _ = deserialize_schedule(&s);
        }
    }

    proptest! {
        #[test]
        fn serialization_roundtrip_proptest(schedule in schedule_strategy()) {
            check_roundtrip(schedule);
        }

        /// Exercise the MTF list bookkeeping harder: long schedules over a small task set, so ranks
        /// stay small, interleaved with occasional first-time tasks.
        #[test]
        fn serialization_roundtrip_hot_set_proptest(
            schedule in (any::<u64>(), vec(prop_oneof![
                Just(ScheduleStep::Random),
                (0usize..8).prop_map(|tid| ScheduleStep::Task(TaskId::from(tid))),
                (0usize..100).prop_map(|tid| ScheduleStep::Task(TaskId::from(tid))),
            ], (0, 2000)))
                .prop_map(|(seed, steps)| Schedule { seed, steps })
        ) {
            check_roundtrip(schedule);
        }

        /// Arbitrary bytes must never panic the deserializer.
        #[test]
        fn deserialize_arbitrary_bytes_does_not_panic(bytes in vec(any::<u8>(), (0, 200))) {
            let _ = deserialize_schedule(&hex::encode(bytes));
        }
    }
}
