use veyra_pricing::{
    MAX_PRICE_DAYS, MAX_QUOTE_NIGHTS, MoneyMicros, OccupancyAdjustment, PriceVector, PricingError,
};

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

#[test]
fn vector_and_stay_boundaries_are_deterministic() {
    assert_eq!(
        PriceVector::try_new(0, Vec::new(), 1),
        Err(PricingError::InvalidVectorLength(0))
    );
    assert_eq!(
        PriceVector::try_new(0, vec![money(1); MAX_PRICE_DAYS + 1], 1),
        Err(PricingError::InvalidVectorLength(MAX_PRICE_DAYS + 1))
    );
    assert_eq!(
        PriceVector::try_new(0, vec![MoneyMicros::signed(-1)], 1),
        Err(PricingError::NegativeAuthoritativeComponent)
    );
    assert_eq!(
        PriceVector::try_new(0, vec![money(1)], 0),
        Err(PricingError::UnknownModelVersion)
    );

    let vector = PriceVector::try_new(0, vec![money(1); MAX_PRICE_DAYS], 3)
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(vector.model_version(), 3);
    let zero = OccupancyAdjustment {
        per_adult_per_night: money(0),
        per_child_per_night: money(0),
    };
    assert_eq!(
        vector.quote(0, 0, 1, 0, zero),
        Err(PricingError::InvalidStay)
    );
    assert_eq!(
        vector.quote(1, 0, 1, 0, zero),
        Err(PricingError::InvalidStay)
    );
    assert_eq!(
        vector.quote(
            0,
            i32::try_from(MAX_QUOTE_NIGHTS + 1).unwrap_or_default(),
            1,
            0,
            zero
        ),
        Err(PricingError::StayTooLong(MAX_QUOTE_NIGHTS + 1))
    );
    assert_eq!(
        vector.quote(-1, 1, 1, 0, zero),
        Err(PricingError::OutsidePriceHorizon)
    );
    assert_eq!(
        vector.quote(729, 731, 1, 0, zero),
        Err(PricingError::OutsidePriceHorizon)
    );

    let adjustment = OccupancyAdjustment {
        per_adult_per_night: money(10),
        per_child_per_night: money(4),
    };
    let projected = vector
        .quote(5, 8, 2, 1, adjustment)
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(projected.base.get(), 3);
    assert_eq!(projected.occupancy_adjustment.get(), 72);
    assert_eq!(projected.total.get(), 75);
    assert_eq!(projected.model_version, 3);
}

#[test]
fn money_checked_arithmetic_is_explicit() {
    assert_eq!(
        MoneyMicros::try_nonnegative(-1),
        Err(PricingError::NegativeAuthoritativeComponent)
    );
    assert_eq!(MoneyMicros::signed(-5).get(), -5);
    assert_eq!(
        money(7)
            .checked_add(money(8))
            .unwrap_or_else(|_| unreachable!())
            .get(),
        15
    );
    assert_eq!(
        money(7)
            .checked_mul(3)
            .unwrap_or_else(|_| unreachable!())
            .get(),
        21
    );
    assert_eq!(
        MoneyMicros::signed(i64::MAX).checked_add(money(1)),
        Err(PricingError::Overflow)
    );
    assert_eq!(
        MoneyMicros::signed(i64::MAX).checked_mul(2),
        Err(PricingError::Overflow)
    );

    let vector = PriceVector::try_new(0, vec![money(1)], 1).unwrap_or_else(|_| unreachable!());
    let negative = OccupancyAdjustment {
        per_adult_per_night: MoneyMicros::signed(-2),
        per_child_per_night: money(0),
    };
    assert_eq!(
        vector.quote(0, 1, 1, 0, negative),
        Err(PricingError::NegativeProjectedTotal)
    );
}

#[test]
fn every_pricing_error_has_stable_display() {
    for error in [
        PricingError::NegativeAuthoritativeComponent,
        PricingError::NegativeProjectedTotal,
        PricingError::Overflow,
        PricingError::InvalidVectorLength(0),
        PricingError::UnknownModelVersion,
        PricingError::InvalidStay,
        PricingError::StayTooLong(91),
        PricingError::OutsidePriceHorizon,
    ] {
        assert!(!error.to_string().is_empty());
    }
}
