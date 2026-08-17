use veyra_availability::{AvailabilityError, AvailabilityIndex};

#[test]
fn dense_bitmap_cross_word_transitions_are_exact() {
    let mut index = AvailabilityIndex::new(100, 3, 130).unwrap_or_else(|_| unreachable!());
    assert_eq!(index.start_day(), 100);
    assert_eq!(index.day_count(), 3);
    assert_eq!(index.room_count(), 130);

    let empty = index
        .available_for_stay(100, 101)
        .unwrap_or_else(|_| unreachable!());
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert!(empty.room_ids().is_empty());
    assert_eq!((&empty).into_iter().next(), None);

    for room in [0, 63, 64, 65, 127, 128, 129] {
        for day in 100..103 {
            index
                .set_available(day, room, true)
                .unwrap_or_else(|_| unreachable!());
        }
    }
    index
        .set_available(101, 65, false)
        .unwrap_or_else(|_| unreachable!());

    let two_nights = index
        .available_for_stay(100, 102)
        .unwrap_or_else(|_| unreachable!());
    assert!(!two_nights.is_empty());
    assert_eq!(two_nights.len(), 6);
    assert!(!two_nights.contains(65));
    assert!(!two_nights.contains(130));
    assert_eq!(two_nights.room_ids(), vec![0, 63, 64, 127, 128, 129]);
    assert_eq!(
        (&two_nights).into_iter().collect::<Vec<_>>(),
        vec![0, 63, 64, 127, 128, 129]
    );
    assert_eq!(
        index
            .reference_available_for_stay(100, 102)
            .unwrap_or_else(|_| unreachable!()),
        two_nights.room_ids()
    );
}

#[test]
fn dimensions_ranges_and_display_fail_closed() {
    for (result, expected) in [
        (
            AvailabilityIndex::new(0, 0, 1),
            AvailabilityError::InvalidDayCount(0),
        ),
        (
            AvailabilityIndex::new(0, 731, 1),
            AvailabilityError::InvalidDayCount(731),
        ),
        (
            AvailabilityIndex::new(0, 1, 0),
            AvailabilityError::InvalidRoomCount(0),
        ),
        (
            AvailabilityIndex::new(0, 1, 1_000_001),
            AvailabilityError::InvalidRoomCount(1_000_001),
        ),
    ] {
        assert_eq!(result, Err(expected));
    }

    let mut index = AvailabilityIndex::new(10, 2, 2).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        index.set_available(9, 0, true),
        Err(AvailabilityError::DayOutOfRange(9))
    );
    assert_eq!(
        index.set_available(12, 0, true),
        Err(AvailabilityError::DayOutOfRange(12))
    );
    assert_eq!(
        index.set_available(10, 2, true),
        Err(AvailabilityError::RoomOutOfRange(2))
    );
    assert_eq!(
        index.available_for_stay(10, 10),
        Err(AvailabilityError::InvalidStayRange)
    );
    assert_eq!(
        index.available_for_stay(11, 10),
        Err(AvailabilityError::InvalidStayRange)
    );
    assert_eq!(
        index.available_for_stay(10, 101),
        Err(AvailabilityError::StayTooLong(91))
    );
    assert_eq!(
        index.available_for_stay(9, 10),
        Err(AvailabilityError::DayOutOfRange(9))
    );
    assert_eq!(
        index.available_for_stay(10, 13),
        Err(AvailabilityError::DayOutOfRange(12))
    );

    for error in [
        AvailabilityError::InvalidDayCount(1),
        AvailabilityError::InvalidRoomCount(2),
        AvailabilityError::InvalidStayRange,
        AvailabilityError::StayTooLong(3),
        AvailabilityError::DayOutOfRange(4),
        AvailabilityError::RoomOutOfRange(5),
    ] {
        assert!(!error.to_string().is_empty());
    }
}
