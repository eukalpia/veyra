#![forbid(unsafe_code)]

//! Deterministic fixed-point projected pricing.
//!
//! These values are search projections only. PostgreSQL/Booking Asia remains authoritative at
//! checkout. Floating point is intentionally absent from all price arithmetic.

use core::fmt;

pub const MAX_PRICE_DAYS: usize = 730;
pub const MAX_QUOTE_NIGHTS: usize = 90;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MoneyMicros(i64);

impl MoneyMicros {
    pub fn try_nonnegative(value: i64) -> Result<Self, PricingError> {
        if value < 0 {
            Err(PricingError::NegativeAuthoritativeComponent)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub const fn signed(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, PricingError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(PricingError::Overflow)
    }

    pub fn checked_mul(self, count: u32) -> Result<Self, PricingError> {
        self.0
            .checked_mul(i64::from(count))
            .map(Self)
            .ok_or(PricingError::Overflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OccupancyAdjustment {
    pub per_adult_per_night: MoneyMicros,
    pub per_child_per_night: MoneyMicros,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedPrice {
    pub base: MoneyMicros,
    pub occupancy_adjustment: MoneyMicros,
    pub total: MoneyMicros,
    pub model_version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceVector {
    start_day: i32,
    nightly: Vec<MoneyMicros>,
    model_version: u32,
}

impl PriceVector {
    pub fn try_new(
        start_day: i32,
        nightly: Vec<MoneyMicros>,
        model_version: u32,
    ) -> Result<Self, PricingError> {
        if nightly.is_empty() || nightly.len() > MAX_PRICE_DAYS {
            return Err(PricingError::InvalidVectorLength(nightly.len()));
        }
        if nightly.iter().any(|price| price.get() < 0) {
            return Err(PricingError::NegativeAuthoritativeComponent);
        }
        if model_version == 0 {
            return Err(PricingError::UnknownModelVersion);
        }
        Ok(Self {
            start_day,
            nightly,
            model_version,
        })
    }

    #[must_use]
    pub const fn model_version(&self) -> u32 {
        self.model_version
    }

    /// Computes an exact search price projection for a bounded stay.
    pub fn quote(
        &self,
        check_in_day: i32,
        check_out_day: i32,
        adults: u16,
        children: u16,
        adjustment: OccupancyAdjustment,
    ) -> Result<ProjectedPrice, PricingError> {
        let nights_i64 = i64::from(check_out_day) - i64::from(check_in_day);
        if nights_i64 <= 0 {
            return Err(PricingError::InvalidStay);
        }
        let nights = usize::try_from(nights_i64).map_err(|_| PricingError::InvalidStay)?;
        if nights > MAX_QUOTE_NIGHTS {
            return Err(PricingError::StayTooLong(nights));
        }
        let start_i64 = i64::from(check_in_day) - i64::from(self.start_day);
        if start_i64 < 0 {
            return Err(PricingError::OutsidePriceHorizon);
        }
        let start = usize::try_from(start_i64).map_err(|_| PricingError::OutsidePriceHorizon)?;
        let end = start.checked_add(nights).ok_or(PricingError::Overflow)?;
        let slice = self
            .nightly
            .get(start..end)
            .ok_or(PricingError::OutsidePriceHorizon)?;

        let mut base = MoneyMicros::signed(0);
        for price in slice {
            base = base.checked_add(*price)?;
        }
        let adults = u32::from(adults);
        let children = u32::from(children);
        let per_night = adjustment
            .per_adult_per_night
            .checked_mul(adults)?
            .checked_add(adjustment.per_child_per_night.checked_mul(children)?)?;
        let night_count = u32::try_from(nights).map_err(|_| PricingError::Overflow)?;
        let occupancy_adjustment = per_night.checked_mul(night_count)?;
        let total = base.checked_add(occupancy_adjustment)?;
        if total.get() < 0 {
            return Err(PricingError::NegativeProjectedTotal);
        }
        Ok(ProjectedPrice {
            base,
            occupancy_adjustment,
            total,
            model_version: self.model_version,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PricingError {
    NegativeAuthoritativeComponent,
    NegativeProjectedTotal,
    Overflow,
    InvalidVectorLength(usize),
    UnknownModelVersion,
    InvalidStay,
    StayTooLong(usize),
    OutsidePriceHorizon,
}

impl fmt::Display for PricingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for PricingError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn money(value: i64) -> MoneyMicros {
        MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn exact_quote_uses_fixed_point_checked_math() {
        let vector = PriceVector::try_new(10, vec![money(100), money(200), money(300)], 7)
            .unwrap_or_else(|_| unreachable!());
        let quote = vector
            .quote(
                10,
                12,
                2,
                1,
                OccupancyAdjustment {
                    per_adult_per_night: money(10),
                    per_child_per_night: MoneyMicros::signed(-5),
                },
            )
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            quote,
            ProjectedPrice {
                base: money(300),
                occupancy_adjustment: money(30),
                total: money(330),
                model_version: 7,
            }
        );
        assert_eq!(vector.model_version(), 7);
    }

    #[test]
    fn price_inputs_and_horizon_fail_closed() {
        assert_eq!(
            MoneyMicros::try_nonnegative(-1),
            Err(PricingError::NegativeAuthoritativeComponent)
        );
        assert_eq!(
            PriceVector::try_new(0, Vec::new(), 1),
            Err(PricingError::InvalidVectorLength(0))
        );
        assert_eq!(
            PriceVector::try_new(0, vec![MoneyMicros::signed(-1)], 1),
            Err(PricingError::NegativeAuthoritativeComponent)
        );
        assert_eq!(
            PriceVector::try_new(0, vec![money(1)], 0),
            Err(PricingError::UnknownModelVersion)
        );
        let vector = PriceVector::try_new(10, vec![money(1); 2], 1)
            .unwrap_or_else(|_| unreachable!());
        let zero = OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(0),
        };
        assert_eq!(vector.quote(10, 10, 1, 0, zero), Err(PricingError::InvalidStay));
        assert_eq!(
            vector.quote(9, 10, 1, 0, zero),
            Err(PricingError::OutsidePriceHorizon)
        );
        assert_eq!(
            vector.quote(10, 13, 1, 0, zero),
            Err(PricingError::OutsidePriceHorizon)
        );
    }

    #[test]
    fn overflow_and_negative_total_are_rejected() {
        assert_eq!(
            MoneyMicros::signed(i64::MAX).checked_add(money(1)),
            Err(PricingError::Overflow)
        );
        assert_eq!(
            MoneyMicros::signed(i64::MAX).checked_mul(2),
            Err(PricingError::Overflow)
        );
        let vector = PriceVector::try_new(0, vec![money(1)], 1)
            .unwrap_or_else(|_| unreachable!());
        let negative = OccupancyAdjustment {
            per_adult_per_night: MoneyMicros::signed(-2),
            per_child_per_night: money(0),
        };
        assert_eq!(
            vector.quote(0, 1, 1, 0, negative),
            Err(PricingError::NegativeProjectedTotal)
        );
        assert_eq!(PricingError::Overflow.to_string(), "Overflow");
    }
}
