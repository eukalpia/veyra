#![forbid(unsafe_code)]

//! Deterministic baseline availability intersection over compact room IDs.

use core::fmt;

const MAX_INDEX_DAYS: u32 = 730;
const MAX_ROOM_IDS: u32 = 1_000_000;
const MAX_STAY_NIGHTS: u32 = 90;
const WORD_BITS: u32 = 64;

/// Dense immutable set of internal room IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DenseRoomSet {
    words: Vec<u64>,
    room_count: u32,
}

impl DenseRoomSet {
    #[must_use]
    pub fn contains(&self, room_id: u32) -> bool {
        if room_id >= self.room_count { return false; }
        let word = usize::try_from(room_id / WORD_BITS).unwrap_or(usize::MAX);
        let bit = room_id % WORD_BITS;
        self.words.get(word).is_some_and(|value| value & (1_u64 << bit) != 0)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.words.iter().map(|word| word.count_ones() as usize).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool { self.words.iter().all(|word| *word == 0) }

    #[must_use]
    pub fn room_ids(&self) -> Vec<u32> {
        (0..self.room_count).filter(|id| self.contains(*id)).collect()
    }
}

/// Immutable per-night dense availability index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvailabilityIndex {
    start_day: u32,
    day_count: u32,
    room_count: u32,
    words_per_day: usize,
    words: Vec<u64>,
}

impl AvailabilityIndex {
    pub fn new(start_day: u32, day_count: u32, room_count: u32) -> Result<Self, AvailabilityError> {
        if day_count == 0 || day_count > MAX_INDEX_DAYS {
            return Err(AvailabilityError::InvalidDayCount(day_count));
        }
        if room_count == 0 || room_count > MAX_ROOM_IDS {
            return Err(AvailabilityError::InvalidRoomCount(room_count));
        }
        let words_per_day_u32 = room_count
            .checked_add(WORD_BITS - 1)
            .ok_or(AvailabilityError::SizeOverflow)?
            / WORD_BITS;
        let words_per_day = usize::try_from(words_per_day_u32).map_err(|_| AvailabilityError::SizeOverflow)?;
        let days = usize::try_from(day_count).map_err(|_| AvailabilityError::SizeOverflow)?;
        let total_words = words_per_day.checked_mul(days).ok_or(AvailabilityError::SizeOverflow)?;
        Ok(Self { start_day, day_count, room_count, words_per_day, words: vec![0; total_words] })
    }

    pub fn set_available(&mut self, day: u32, room_id: u32, available: bool) -> Result<(), AvailabilityError> {
        let day_offset = self.day_offset(day)?;
        if room_id >= self.room_count { return Err(AvailabilityError::RoomOutOfRange(room_id)); }
        let word_in_day = usize::try_from(room_id / WORD_BITS).map_err(|_| AvailabilityError::SizeOverflow)?;
        let bit = room_id % WORD_BITS;
        let index = day_offset
            .checked_mul(self.words_per_day)
            .and_then(|base| base.checked_add(word_in_day))
            .ok_or(AvailabilityError::SizeOverflow)?;
        let word = self.words.get_mut(index).ok_or(AvailabilityError::InternalBounds)?;
        if available { *word |= 1_u64 << bit; } else { *word &= !(1_u64 << bit); }
        Ok(())
    }

    /// Returns rooms available on every night in `[check_in, check_out)`.
    pub fn available_for_stay(&self, check_in: u32, check_out: u32) -> Result<DenseRoomSet, AvailabilityError> {
        let nights = check_out.checked_sub(check_in).ok_or(AvailabilityError::InvalidStayRange)?;
        if nights == 0 { return Err(AvailabilityError::InvalidStayRange); }
        if nights > MAX_STAY_NIGHTS { return Err(AvailabilityError::StayTooLong(nights)); }
        let first = self.day_slice(check_in)?;
        let last_night = check_out.checked_sub(1).ok_or(AvailabilityError::InvalidStayRange)?;
        let _ = self.day_offset(last_night)?;
        let mut result = first.to_vec();
        for day in check_in.saturating_add(1)..check_out {
            for (target, source) in result.iter_mut().zip(self.day_slice(day)?) {
                *target &= *source;
            }
        }
        Ok(DenseRoomSet { words: result, room_count: self.room_count })
    }

    /// Deliberately slow reference implementation for differential correctness testing.
    pub fn reference_available_for_stay(&self, check_in: u32, check_out: u32) -> Result<Vec<u32>, AvailabilityError> {
        let _ = self.available_for_stay(check_in, check_out)?;
        let mut result = Vec::new();
        for room_id in 0..self.room_count {
            let mut valid = true;
            for day in check_in..check_out {
                if !self.available_on_day(day, room_id)? {
                    valid = false;
                    break;
                }
            }
            if valid { result.push(room_id); }
        }
        Ok(result)
    }

    #[must_use]
    pub const fn start_day(&self) -> u32 { self.start_day }
    #[must_use]
    pub const fn day_count(&self) -> u32 { self.day_count }
    #[must_use]
    pub const fn room_count(&self) -> u32 { self.room_count }

    fn available_on_day(&self, day: u32, room_id: u32) -> Result<bool, AvailabilityError> {
        if room_id >= self.room_count { return Err(AvailabilityError::RoomOutOfRange(room_id)); }
        let slice = self.day_slice(day)?;
        let word = usize::try_from(room_id / WORD_BITS).map_err(|_| AvailabilityError::SizeOverflow)?;
        let bit = room_id % WORD_BITS;
        Ok(slice.get(word).is_some_and(|value| value & (1_u64 << bit) != 0))
    }

    fn day_slice(&self, day: u32) -> Result<&[u64], AvailabilityError> {
        let offset = self.day_offset(day)?;
        let start = offset.checked_mul(self.words_per_day).ok_or(AvailabilityError::SizeOverflow)?;
        let end = start.checked_add(self.words_per_day).ok_or(AvailabilityError::SizeOverflow)?;
        self.words.get(start..end).ok_or(AvailabilityError::InternalBounds)
    }

    fn day_offset(&self, day: u32) -> Result<usize, AvailabilityError> {
        let offset = day.checked_sub(self.start_day).ok_or(AvailabilityError::DayOutOfRange(day))?;
        if offset >= self.day_count { return Err(AvailabilityError::DayOutOfRange(day)); }
        usize::try_from(offset).map_err(|_| AvailabilityError::SizeOverflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvailabilityError {
    InvalidDayCount(u32),
    InvalidRoomCount(u32),
    InvalidStayRange,
    StayTooLong(u32),
    DayOutOfRange(u32),
    RoomOutOfRange(u32),
    SizeOverflow,
    InternalBounds,
}

impl fmt::Display for AvailabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { write!(formatter, "{self:?}") }
}
impl std::error::Error for AvailabilityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn stay_intersection_matches_expected_rooms() {
        let mut index = AvailabilityIndex::new(100, 4, 5).unwrap_or_else(|_| unreachable!());
        for day in 100..104 {
            index.set_available(day, 1, true).unwrap_or_else(|_| unreachable!());
        }
        for day in 100..103 {
            index.set_available(day, 2, true).unwrap_or_else(|_| unreachable!());
        }
        index.set_available(101, 4, true).unwrap_or_else(|_| unreachable!());
        let result = index.available_for_stay(100, 103).unwrap_or_else(|_| unreachable!());
        assert_eq!(result.room_ids(), vec![1, 2]);
        assert_eq!(result.len(), 2);
        assert!(result.contains(1));
        assert!(!result.contains(4));
        assert!(!result.contains(9));
    }

    #[test]
    fn invalid_dimensions_and_queries_fail_closed() {
        assert_eq!(AvailabilityIndex::new(0, 0, 1), Err(AvailabilityError::InvalidDayCount(0)));
        assert_eq!(AvailabilityIndex::new(0, 1, 0), Err(AvailabilityError::InvalidRoomCount(0)));
        let mut index = AvailabilityIndex::new(10, 2, 2).unwrap_or_else(|_| unreachable!());
        assert_eq!(index.set_available(9, 0, true), Err(AvailabilityError::DayOutOfRange(9)));
        assert_eq!(index.set_available(10, 2, true), Err(AvailabilityError::RoomOutOfRange(2)));
        assert_eq!(index.available_for_stay(10, 10), Err(AvailabilityError::InvalidStayRange));
        assert_eq!(index.available_for_stay(11, 10), Err(AvailabilityError::InvalidStayRange));
        assert!(matches!(index.available_for_stay(10, 101), Err(AvailabilityError::StayTooLong(91))));
        assert!(matches!(index.available_for_stay(10, 13), Err(AvailabilityError::DayOutOfRange(12))));
    }

    #[test]
    fn getters_and_empty_result_are_exact() {
        let index = AvailabilityIndex::new(7, 3, 65).unwrap_or_else(|_| unreachable!());
        assert_eq!((index.start_day(), index.day_count(), index.room_count()), (7, 3, 65));
        let result = index.available_for_stay(7, 8).unwrap_or_else(|_| unreachable!());
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert_eq!(result.room_ids(), Vec::<u32>::new());
    }

    proptest! {
        #[test]
        fn optimized_matches_reference(
            room_count in 1_u32..65,
            day_count in 1_u32..20,
            seed in any::<u64>(),
            requested_start in any::<u32>(),
            requested_nights in 1_u32..20,
        ) {
            let start_day = 1000_u32;
            let mut index = AvailabilityIndex::new(start_day, day_count, room_count)
                .unwrap_or_else(|_| unreachable!());
            for day_offset in 0..day_count {
                for room in 0..room_count {
                    let mixed = seed
                        .wrapping_add(u64::from(day_offset).wrapping_mul(0x9e37_79b9))
                        .wrapping_add(u64::from(room).wrapping_mul(0x85eb_ca6b));
                    let available = mixed.rotate_left(room % 63) & 3 != 0;
                    index.set_available(start_day + day_offset, room, available)
                        .unwrap_or_else(|_| unreachable!());
                }
            }
            let max_start = day_count - 1;
            let offset = requested_start % (max_start + 1);
            let max_nights = day_count - offset;
            let nights = requested_nights.min(max_nights).min(MAX_STAY_NIGHTS);
            let check_in = start_day + offset;
            let check_out = check_in + nights;
            let fast = index.available_for_stay(check_in, check_out)
                .unwrap_or_else(|_| unreachable!())
                .room_ids();
            let reference = index.reference_available_for_stay(check_in, check_out)
                .unwrap_or_else(|_| unreachable!());
            prop_assert_eq!(fast, reference);
        }
    }
}
