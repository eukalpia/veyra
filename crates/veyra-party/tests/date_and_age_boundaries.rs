use veyra_party::{AgeEvidence, BookingParty, CivilDate, PartyError, Traveler, TravelerId};

fn date(year: i32, month: u8, day: u8) -> CivilDate {
    CivilDate::new(year, month, day).unwrap_or_else(|_| unreachable!())
}

#[test]
fn civil_date_rejects_both_month_and_day_boundaries() {
    assert_eq!(CivilDate::new(2026, 0, 1), Err(PartyError::InvalidMonth(0)));
    assert_eq!(
        CivilDate::new(2026, 13, 1),
        Err(PartyError::InvalidMonth(13))
    );
    assert_eq!(
        CivilDate::new(2026, 1, 0),
        Err(PartyError::InvalidDay {
            year: 2026,
            month: 1,
            day: 0,
        })
    );
    assert_eq!(
        CivilDate::new(2026, 2, 29),
        Err(PartyError::InvalidDay {
            year: 2026,
            month: 2,
            day: 29,
        })
    );
    assert!(CivilDate::new(2024, 2, 29).is_ok());
    assert_eq!(
        CivilDate::new(1900, 2, 29),
        Err(PartyError::InvalidDay {
            year: 1900,
            month: 2,
            day: 29,
        })
    );
    assert!(CivilDate::new(2000, 2, 29).is_ok());
}

#[test]
fn age_is_checked_before_birthday_on_birthday_and_after_birthday() {
    let birth = date(2000, 9, 21);
    assert_eq!(
        birth.age_on(date(1999, 9, 21)),
        Err(PartyError::CheckInBeforeBirth)
    );
    assert_eq!(birth.age_on(date(2026, 9, 20)), Ok(25));
    assert_eq!(birth.age_on(date(2026, 9, 21)), Ok(26));
    assert_eq!(birth.age_on(date(2026, 9, 22)), Ok(26));
}

#[test]
fn extreme_year_distance_fails_closed_without_integer_overflow() {
    assert_eq!(
        date(i32::MIN, 1, 1).age_on(date(i32::MAX, 1, 1)),
        Err(PartyError::AgeOutOfRange)
    );
}

#[test]
fn explicit_and_derived_ages_share_the_same_hard_upper_bound() {
    let check_in = date(2026, 9, 21);
    assert_eq!(AgeEvidence::AgeAtCheckIn(130).age_at(check_in), Ok(130));
    assert_eq!(
        AgeEvidence::AgeAtCheckIn(131).age_at(check_in),
        Err(PartyError::AgeOutOfRange)
    );
    assert_eq!(date(1896, 9, 21).age_on(check_in), Ok(130));
    assert_eq!(
        date(1895, 9, 21).age_on(check_in),
        Err(PartyError::AgeOutOfRange)
    );
}

#[test]
fn party_age_projection_preserves_traveler_order_and_evidence() {
    let check_in = date(2026, 9, 21);
    let mut builder = BookingParty::builder();
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(2),
            AgeEvidence::AgeAtCheckIn(40),
            false,
        ))
        .unwrap_or_else(|_| unreachable!());
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(1),
            AgeEvidence::BirthDate(date(2016, 9, 21)),
            true,
        ))
        .unwrap_or_else(|_| unreachable!());
    let party = builder.build().unwrap_or_else(|_| unreachable!());
    assert_eq!(
        party.ages_at(check_in),
        Ok(vec![(TravelerId::new(1), 10), (TravelerId::new(2), 40)])
    );
}
