use veyra_restrictions::{
    CompiledRestrictions, RestrictionError, RestrictionRule, RESTRICTION_SCHEMA_V1, compile,
};

fn compile_bounds(minimum: u16, maximum: u16) -> Result<CompiledRestrictions, RestrictionError> {
    compile(
        RESTRICTION_SCHEMA_V1,
        &[
            RestrictionRule::MinStay(minimum),
            RestrictionRule::MaxStay(maximum),
        ],
    )
}

#[test]
fn every_invalid_bound_predicate_is_independently_exercised() {
    assert_eq!(
        compile_bounds(0, 1),
        Err(RestrictionError::InvalidStayBounds {
            minimum: 0,
            maximum: 1,
        })
    );
    assert_eq!(
        compile_bounds(1, 0),
        Err(RestrictionError::InvalidStayBounds {
            minimum: 1,
            maximum: 0,
        })
    );
    assert_eq!(
        compile_bounds(3, 2),
        Err(RestrictionError::InvalidStayBounds {
            minimum: 3,
            maximum: 2,
        })
    );
    assert_eq!(
        compile_bounds(1, 366),
        Err(RestrictionError::InvalidStayBounds {
            minimum: 1,
            maximum: 366,
        })
    );
}

#[test]
fn compiled_metadata_and_every_stay_boundary_are_observable() {
    let policy = compile(
        RESTRICTION_SCHEMA_V1,
        &[
            RestrictionRule::MinStay(2),
            RestrictionRule::MaxStay(5),
            RestrictionRule::ClosedToArrival(10),
            RestrictionRule::ClosedToDeparture(20),
        ],
    )
    .unwrap_or_else(|_| unreachable!());

    assert_eq!(policy.schema_version(), RESTRICTION_SCHEMA_V1);
    assert_eq!(policy.min_stay(), 2);
    assert_eq!(policy.max_stay(), 5);
    assert_eq!(policy.validate_stay(1, 3), Ok(2));
    assert_eq!(
        policy.validate_stay(10, 12),
        Err(RestrictionError::ClosedToArrival(10))
    );
    assert_eq!(
        policy.validate_stay(15, 20),
        Err(RestrictionError::ClosedToDeparture(20))
    );
    assert_eq!(
        policy.validate_stay(i32::MIN, i32::MAX),
        Err(RestrictionError::StayTooLong)
    );
}
