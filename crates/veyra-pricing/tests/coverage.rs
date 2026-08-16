use veyra_pricing::{
    MAX_PRICE_DAYS, MAX_QUOTE_NIGHTS, MoneyMicros, OccupancyAdjustment, PriceVector, PricingError,
};

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

#[test]
fn vector_and_stay_boundaries_are_deterministic() {
    assert_eq!(
        PriceVector::try_new(0, vec![money(1); MAX_PRICE_DAYS + 1], 1),
        Err(PricingError::InvalidVectorLength(MAX_PRICE_DAYS + 1))
    );
    let vector = PriceVector::try_new(0, vec![money(1); MAX_PRICE_DAYS], 3)
        .unwrap_or_else(|_| unreachable!());
    let zero = OccupancyAdjustment {
        per_adult_per_night: money(0),
        per_child_per_night: money(0),
    };
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
