use veyra_pricing::MoneyMicros;
use veyra_ranking::{
    MAX_RANK_CANDIDATES, RankCandidate, RankingError, RankingKind, RankingProfile, top_k,
};

fn candidate(room_id: u32) -> RankCandidate {
    RankCandidate {
        property_id: 1,
        room_id,
        projected_price: MoneyMicros::try_nonnegative(100).unwrap_or_else(|_| unreachable!()),
        distance_meters: 10,
        quality_milli: 500,
        flexibility_milli: 500,
        family_penalty: 0,
    }
}

#[test]
fn candidate_cardinality_is_bounded_before_sorting() {
    let candidates = vec![candidate(1); MAX_RANK_CANDIDATES + 1];
    assert_eq!(
        top_k(RankingProfile::v1(RankingKind::Cheapest), &candidates, 1),
        Err(RankingError::TooManyCandidates(MAX_RANK_CANDIDATES + 1))
    );
}

#[test]
fn zero_limit_and_flexibility_validation_are_deterministic() {
    assert_eq!(
        top_k(
            RankingProfile::v1(RankingKind::BestValue),
            &[candidate(1)],
            0
        ),
        Ok(Vec::new())
    );
    let invalid = RankCandidate {
        flexibility_milli: 1_001,
        ..candidate(2)
    };
    assert_eq!(
        top_k(
            RankingProfile::v1(RankingKind::BestForFamily),
            &[invalid],
            1
        ),
        Err(RankingError::InvalidNormalizedSignal(2))
    );
    for error in [
        RankingError::UnsupportedVersion(2),
        RankingError::TooManyCandidates(3),
        RankingError::LimitTooLarge(4),
        RankingError::NegativeProjectedPrice(5),
        RankingError::InvalidNormalizedSignal(6),
    ] {
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn bounded_heap_replaces_only_the_current_worst_candidate() {
    let ranked = top_k(
        RankingProfile::v1(RankingKind::Cheapest),
        &[candidate(10), candidate(20), candidate(5), candidate(30)],
        2,
    )
    .unwrap_or_else(|_| unreachable!());

    assert_eq!(
        ranked
            .iter()
            .map(|entry| entry.candidate.room_id)
            .collect::<Vec<_>>(),
        vec![5, 10]
    );
}
