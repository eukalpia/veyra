use veyra_pricing::{
    MoneyMicros, OccupancyAdjustment, PriceVector, PricingError,
};

fn adjustment(adult: i64, child: i64) -> OccupancyAdjustment {
    OccupancyAdjustment {
        per_adult_per_night: MoneyMicros::signed(adult),
        per_child_per_night: MoneyMicros::signed(child),
    }
}

fn vector(prices: &[i64]) -> PriceVector {
    PriceVector::try_new(
        0,
        prices.iter().copied().map(MoneyMicros::signed).collect(),
        1,
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn every_quote_arithmetic_stage_fails_closed_independently() {
    assert_eq!(
        vector(&[i64::MAX, 1]).quote(0, 2, 0, 0, adjustment(0, 0)),
        Err(PricingError::Overflow),
        "base nightly accumulation must be checked"
    );

    assert_eq!(
        vector(&[0]).quote(0, 1, 2, 0, adjustment(i64::MAX, 0)),
        Err(PricingError::Overflow),
        "adult multiplier must be checked"
    );

    assert_eq!(
        vector(&[0]).quote(0, 1, 0, 2, adjustment(0, i64::MAX)),
        Err(PricingError::Overflow),
        "child multiplier must be checked"
    );

    assert_eq!(
        vector(&[0]).quote(0, 1, 1, 1, adjustment(i64::MAX, 1)),
        Err(PricingError::Overflow),
        "adult and child adjustments must be added with checked arithmetic"
    );

    assert_eq!(
        vector(&[0, 0]).quote(0, 2, 1, 0, adjustment(i64::MAX / 2 + 1, 0)),
        Err(PricingError::Overflow),
        "per-night occupancy adjustment must be checked across the stay"
    );

    assert_eq!(
        vector(&[i64::MAX]).quote(0, 1, 1, 0, adjustment(1, 0)),
        Err(PricingError::Overflow),
        "base and occupancy totals must be added with checked arithmetic"
    );

    assert_eq!(
        vector(&[0]).quote(0, 1, 1, 0, adjustment(-1, 0)),
        Err(PricingError::NegativeProjectedTotal),
        "a mathematically valid negative projection is still invalid travel pricing"
    );
}

#[test]
fn stay_and_horizon_boundaries_reject_before_price_arithmetic() {
    let prices = vec![MoneyMicros::signed(1); 100];
    let vector = PriceVector::try_new(10, prices, 1).unwrap_or_else(|_| unreachable!());

    assert_eq!(
        vector.quote(10, 10, 1, 0, adjustment(i64::MAX, i64::MAX)),
        Err(PricingError::InvalidStay)
    );
    assert_eq!(
        vector.quote(10, 101, 1, 0, adjustment(0, 0)),
        Err(PricingError::StayTooLong(91))
    );
    assert_eq!(
        vector.quote(9, 10, 1, 0, adjustment(0, 0)),
        Err(PricingError::OutsidePriceHorizon)
    );
    assert_eq!(
        vector.quote(109, 110, 1, 0, adjustment(0, 0)),
        Ok(veyra_pricing::ProjectedPrice {
            base: MoneyMicros::signed(1),
            occupancy_adjustment: MoneyMicros::signed(0),
            total: MoneyMicros::signed(1),
            model_version: 1,
        })
    );
    assert_eq!(
        vector.quote(110, 111, 1, 0, adjustment(0, 0)),
        Err(PricingError::OutsidePriceHorizon)
    );
}
