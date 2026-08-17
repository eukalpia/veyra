use veyra_restrictions::{
    MAX_STAY_NIGHTS, RESTRICTION_SCHEMA_V1, RestrictionError, RestrictionRule, compile,
};

#[test]
fn incomplete_duplicate_and_out_of_range_bounds_fail_closed() {
    assert_eq!(
        compile(RESTRICTION_SCHEMA_V1, &[RestrictionRule::MinStay(1)]),
        Err(RestrictionError::MissingRule("MAX_STAY"))
    );
    assert_eq!(
        compile(
            RESTRICTION_SCHEMA_V1,
            &[
                RestrictionRule::MinStay(1),
                RestrictionRule::MaxStay(2),
                RestrictionRule::MaxStay(3),
            ],
        ),
        Err(RestrictionError::DuplicateRule("MAX_STAY"))
    );
    for (minimum, maximum) in [(0, 1), (1, 0), (2, 1), (1, MAX_STAY_NIGHTS + 1)] {
        assert_eq!(
            compile(
                RESTRICTION_SCHEMA_V1,
                &[
                    RestrictionRule::MinStay(minimum),
                    RestrictionRule::MaxStay(maximum),
                ],
            ),
            Err(RestrictionError::InvalidStayBounds { minimum, maximum })
        );
    }
}

#[test]
fn interval_conversion_and_error_surface_are_explicit() {
    let policy = compile(
        RESTRICTION_SCHEMA_V1,
        &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(365)],
    )
    .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        policy.validate_stay(i32::MIN, i32::MAX),
        Err(RestrictionError::StayTooLong)
    );
    for error in [
        RestrictionError::UnsupportedSchema(2),
        RestrictionError::TooManyRules(3),
        RestrictionError::MissingRule("x"),
        RestrictionError::DuplicateRule("x"),
        RestrictionError::InvalidStayBounds {
            minimum: 2,
            maximum: 1,
        },
        RestrictionError::InvalidStayInterval,
        RestrictionError::StayTooLong,
        RestrictionError::BelowMinStay {
            nights: 1,
            minimum: 2,
        },
        RestrictionError::AboveMaxStay {
            nights: 3,
            maximum: 2,
        },
        RestrictionError::ClosedToArrival(4),
        RestrictionError::ClosedToDeparture(5),
    ] {
        assert!(!error.to_string().is_empty());
    }
}
