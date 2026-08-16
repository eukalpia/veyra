#![forbid(unsafe_code)]

//! Deterministic non-ML ranking over candidates that have already passed all hard constraints.
//!
//! Ranking cannot create validity. Every profile is versioned, bounded and has explicit stable
//! tie-breaking. `BestForFamily` uses a lexicographic family-penalty dimension, so monetary scale
//! can never accidentally override the semantic layout priority.

use core::cmp::Reverse;
use core::fmt;
use veyra_pricing::MoneyMicros;

pub const RANKING_VERSION_V1: u32 = 1;
pub const MAX_RANK_CANDIDATES: usize = 1_000_000;
pub const MAX_TOP_K: usize = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RankingKind {
    Cheapest,
    BestValue,
    BestForFamily,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankingProfile {
    pub version: u32,
    pub kind: RankingKind,
}

impl RankingProfile {
    #[must_use]
    pub const fn v1(kind: RankingKind) -> Self {
        Self {
            version: RANKING_VERSION_V1,
            kind,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankCandidate {
    pub property_id: u32,
    pub room_id: u32,
    pub projected_price: MoneyMicros,
    pub distance_meters: u32,
    pub quality_milli: u16,
    pub flexibility_milli: u16,
    pub family_penalty: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankedCandidate {
    pub candidate: RankCandidate,
    pub deterministic_score: i128,
}

pub fn top_k(
    profile: RankingProfile,
    candidates: &[RankCandidate],
    limit: usize,
) -> Result<Vec<RankedCandidate>, RankingError> {
    validate(profile, candidates, limit)?;
    let mut ranked = candidates
        .iter()
        .copied()
        .map(|candidate| RankedCandidate {
            candidate,
            deterministic_score: score(profile.kind, candidate),
        })
        .collect::<Vec<_>>();

    ranked.sort_by_key(|entry| ordering_key(profile.kind, *entry));
    ranked.truncate(limit);
    Ok(ranked)
}

fn validate(
    profile: RankingProfile,
    candidates: &[RankCandidate],
    limit: usize,
) -> Result<(), RankingError> {
    if profile.version != RANKING_VERSION_V1 {
        return Err(RankingError::UnsupportedVersion(profile.version));
    }
    if candidates.len() > MAX_RANK_CANDIDATES {
        return Err(RankingError::TooManyCandidates(candidates.len()));
    }
    if limit > MAX_TOP_K {
        return Err(RankingError::LimitTooLarge(limit));
    }
    for candidate in candidates {
        if candidate.projected_price.get() < 0 {
            return Err(RankingError::NegativeProjectedPrice(candidate.room_id));
        }
        if candidate.quality_milli > 1_000 || candidate.flexibility_milli > 1_000 {
            return Err(RankingError::InvalidNormalizedSignal(candidate.room_id));
        }
    }
    Ok(())
}

fn score(kind: RankingKind, candidate: RankCandidate) -> i128 {
    match kind {
        RankingKind::Cheapest | RankingKind::BestForFamily => {
            i128::from(candidate.projected_price.get())
        }
        RankingKind::BestValue => {
            i128::from(candidate.projected_price.get())
                + i128::from(candidate.distance_meters) * 1_000
                + i128::from(candidate.family_penalty) * 10_000_000
                - i128::from(candidate.quality_milli) * 10_000
                - i128::from(candidate.flexibility_milli) * 5_000
        }
    }
}

fn ordering_key(
    kind: RankingKind,
    entry: RankedCandidate,
) -> (u16, i128, u32, Reverse<u16>, Reverse<u16>, u32, u32) {
    let candidate = entry.candidate;
    let family_penalty = match kind {
        RankingKind::BestForFamily => candidate.family_penalty,
        RankingKind::Cheapest | RankingKind::BestValue => 0,
    };
    (
        family_penalty,
        entry.deterministic_score,
        candidate.distance_meters,
        Reverse(candidate.quality_milli),
        Reverse(candidate.flexibility_milli),
        candidate.property_id,
        candidate.room_id,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RankingError {
    UnsupportedVersion(u32),
    TooManyCandidates(usize),
    LimitTooLarge(usize),
    NegativeProjectedPrice(u32),
    InvalidNormalizedSignal(u32),
}

impl fmt::Display for RankingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for RankingError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn money(value: i64) -> MoneyMicros {
        MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
    }

    fn candidate(
        property_id: u32,
        room_id: u32,
        price: i64,
        family_penalty: u16,
        quality: u16,
    ) -> RankCandidate {
        RankCandidate {
            property_id,
            room_id,
            projected_price: money(price),
            distance_meters: 500,
            quality_milli: quality,
            flexibility_milli: 700,
            family_penalty,
        }
    }

    #[test]
    fn cheapest_is_deterministic_with_explicit_ties() {
        let ranked = top_k(
            RankingProfile::v1(RankingKind::Cheapest),
            &[
                candidate(2, 2, 100, 0, 500),
                candidate(1, 3, 100, 0, 500),
                candidate(1, 1, 80, 0, 500),
            ],
            3,
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            ranked
                .iter()
                .map(|entry| entry.candidate.room_id)
                .collect::<Vec<_>>(),
            vec![1, 3, 2]
        );
    }

    #[test]
    fn family_profile_prioritizes_layout_penalty_lexicographically() {
        let ranked = top_k(
            RankingProfile::v1(RankingKind::BestForFamily),
            &[
                candidate(1, 1, 1, 1, 900),
                candidate(1, 2, i64::MAX / 4, 0, 500),
            ],
            2,
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(ranked[0].candidate.room_id, 2);
        assert_eq!(ranked[0].candidate.family_penalty, 0);
    }

    #[test]
    fn best_value_rewards_quality_without_changing_validity() {
        let ranked = top_k(
            RankingProfile::v1(RankingKind::BestValue),
            &[
                candidate(1, 1, 10_000_000, 0, 100),
                candidate(1, 2, 10_000_000, 0, 900),
            ],
            2,
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(ranked[0].candidate.room_id, 2);
    }

    #[test]
    fn invalid_version_limits_and_signals_fail_closed() {
        assert_eq!(
            top_k(
                RankingProfile {
                    version: 9,
                    kind: RankingKind::Cheapest,
                },
                &[],
                0,
            ),
            Err(RankingError::UnsupportedVersion(9))
        );
        assert_eq!(
            top_k(
                RankingProfile::v1(RankingKind::Cheapest),
                &[],
                MAX_TOP_K + 1,
            ),
            Err(RankingError::LimitTooLarge(MAX_TOP_K + 1))
        );
        let invalid = RankCandidate {
            quality_milli: 1_001,
            ..candidate(1, 1, 1, 0, 500)
        };
        assert_eq!(
            top_k(
                RankingProfile::v1(RankingKind::Cheapest),
                &[invalid],
                1,
            ),
            Err(RankingError::InvalidNormalizedSignal(1))
        );
        let negative = RankCandidate {
            projected_price: MoneyMicros::signed(-1),
            ..candidate(1, 2, 1, 0, 500)
        };
        assert_eq!(
            top_k(
                RankingProfile::v1(RankingKind::Cheapest),
                &[negative],
                1,
            ),
            Err(RankingError::NegativeProjectedPrice(2))
        );
        assert_eq!(
            RankingError::LimitTooLarge(3).to_string(),
            "LimitTooLarge(3)"
        );
    }
}
