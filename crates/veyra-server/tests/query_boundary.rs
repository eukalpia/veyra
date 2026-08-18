use veyra_party::{AgeEvidence, BookingParty, CivilDate, Traveler, TravelerId};
use veyra_query::{MultiRoomStayQuery, SearchEngine, SolutionProfile, SolverConfig};
use veyra_runtime::{CannotProveReason, GenerationState, PublishedGeneration, RuntimeSnapshot};
use veyra_server::{QueryService, ServiceQueryError};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};

fn date(y: i32, m: u8, d: u8) -> CivilDate {
    CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
}

fn party() -> BookingParty {
    let mut builder = BookingParty::builder();
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(1),
            AgeEvidence::AgeAtCheckIn(30),
            false,
        ))
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn query(party: &BookingParty) -> MultiRoomStayQuery<'_> {
    MultiRoomStayQuery {
        destination_id: 1,
        check_in_day: 10,
        check_out_day: 11,
        check_in_date: date(2026, 9, 21),
        party,
        budget: None,
        profile: SolutionProfile::Cheapest,
        solver: SolverConfig::default(),
        limit: 10,
    }
}

#[test]
fn query_service_fails_closed_before_query_generation_is_ready() {
    let initial = PublishedGeneration::<SearchEngine>::try_without_payload(
        RuntimeSnapshot::starting(GenerationId::UNPUBLISHED, ProjectionProgress::ZERO),
    )
    .unwrap_or_else(|_| unreachable!());
    let service = QueryService::new(GenerationState::new(initial));
    let party = party();

    let error = service
        .search_multi_room(LogSequenceNumber::ZERO, &query(&party))
        .err()
        .unwrap_or_else(|| unreachable!());

    assert_eq!(
        error,
        ServiceQueryError::CannotProve(CannotProveReason::NotReady)
    );
    assert_eq!(error.code(), "not_ready");
}

#[test]
fn service_query_error_codes_are_stable_for_admission_failures() {
    let cases = [
        (CannotProveReason::NotReady, "not_ready"),
        (CannotProveReason::StaleProjection, "stale_projection"),
        (CannotProveReason::CdcGap, "cdc_gap"),
        (CannotProveReason::CorruptGeneration, "corrupt_generation"),
        (CannotProveReason::VersionMismatch, "version_mismatch"),
        (
            CannotProveReason::UnsupportedSemantics,
            "unsupported_semantics",
        ),
        (CannotProveReason::Overloaded, "overloaded"),
        (
            CannotProveReason::InternalInvariantFailure,
            "internal_invariant_failure",
        ),
    ];

    for (reason, expected) in cases {
        assert_eq!(ServiceQueryError::CannotProve(reason).code(), expected);
    }
}
