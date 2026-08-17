#![forbid(unsafe_code)]

//! Process boundary and administrative HTTP endpoints.
//!
//! The server is intentionally separate from BEAM/NIF execution. A Veyra process crash
//! therefore cannot directly crash the Elixir VM.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{AddrParseError, SocketAddr};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;
use veyra_query::{
    MultiRoomSearchResult, MultiRoomStayQuery, QueryError, SearchEngine, SearchResult, StayQuery,
};
use veyra_runtime::{
    CannotProveReason, GenerationState, RuntimeSnapshot, RuntimeState, ServicePhase,
};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};

/// Portable default for the administrative listener.
pub const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// Typed query boundary backed only by atomically-published immutable generations.
pub struct QueryService {
    generations: GenerationState<SearchEngine>,
}

impl QueryService {
    /// Creates a query service from atomic generation publication state.
    #[must_use]
    pub const fn new(generations: GenerationState<SearchEngine>) -> Self {
        Self { generations }
    }

    /// Executes the single-room path only after read-your-writes admission succeeds.
    pub fn search(
        &self,
        minimum_lsn: LogSequenceNumber,
        query: &StayQuery<'_>,
    ) -> Result<SearchResult, ServiceQueryError> {
        let engine = self
            .generations
            .admit(minimum_lsn)
            .map_err(ServiceQueryError::CannotProve)?;
        engine.search(query).map_err(ServiceQueryError::Query)
    }

    /// Executes exact multi-room search against the admitted immutable generation.
    pub fn search_multi_room(
        &self,
        minimum_lsn: LogSequenceNumber,
        query: &MultiRoomStayQuery<'_>,
    ) -> Result<MultiRoomSearchResult, ServiceQueryError> {
        let engine = self
            .generations
            .admit(minimum_lsn)
            .map_err(ServiceQueryError::CannotProve)?;
        engine
            .search_multi_room(query)
            .map_err(ServiceQueryError::Query)
    }
}

/// Stable typed failure at the process query boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceQueryError {
    /// Runtime cannot prove that a generation is safe/current enough to serve.
    CannotProve(CannotProveReason),
    /// The admitted generation rejected the typed query itself.
    Query(QueryError),
}

impl ServiceQueryError {
    /// Stable machine-readable category for the Elixir/BFF boundary.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CannotProve(CannotProveReason::NotReady) => "not_ready",
            Self::CannotProve(CannotProveReason::StaleProjection) => "stale_projection",
            Self::CannotProve(CannotProveReason::CdcGap) => "cdc_gap",
            Self::CannotProve(CannotProveReason::CorruptGeneration) => "corrupt_generation",
            Self::CannotProve(CannotProveReason::VersionMismatch) => "version_mismatch",
            Self::CannotProve(CannotProveReason::UnsupportedSemantics)
            | Self::Query(QueryError::MissingRoomTopology(_)) => "unsupported_semantics",
            Self::CannotProve(CannotProveReason::Overloaded) => "overloaded",
            Self::CannotProve(CannotProveReason::InternalInvariantFailure) => {
                "internal_invariant_failure"
            }
            Self::Query(_) => "query_rejected",
        }
    }
}

impl fmt::Display for ServiceQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {self:?}", self.code())
    }
}

impl std::error::Error for ServiceQueryError {}

/// Parses the optional `VEYRA_BIND` value without touching process-global state.
///
/// Keeping this pure makes deployment configuration deterministic and independently testable.
pub fn parse_bind_address(value: Option<&str>) -> Result<SocketAddr, AddrParseError> {
    value.unwrap_or(DEFAULT_BIND).parse()
}

/// Stable health response for operators and the Elixir client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct HealthResponse {
    /// Process liveness.
    pub alive: bool,
    /// Current fail-closed runtime phase.
    pub phase: ServicePhase,
    /// Whether Veyra can currently prove query results.
    pub can_prove_result: bool,
    /// Published immutable generation.
    pub generation: u64,
    /// Applied logical replication position.
    pub applied_lsn: u64,
    /// Published logical replication position.
    pub published_lsn: u64,
}

/// Builds the administrative router.
pub fn router(runtime: RuntimeState) -> Router {
    Router::new()
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .with_state(runtime)
}

/// Returns the initial fail-closed runtime state.
///
/// Bootstrap deliberately never pretends that a query projection exists.
#[must_use]
pub fn bootstrap_runtime() -> RuntimeState {
    RuntimeState::new(RuntimeSnapshot::starting(
        GenerationId::UNPUBLISHED,
        ProjectionProgress::ZERO,
    ))
}

/// Serves the administrative router until `shutdown` resolves.
///
/// A graceful shutdown boundary is part of the public server contract so tests and
/// production supervisors can prove that listener shutdown completes without aborting
/// the process or exposing a half-mutated runtime state.
pub async fn serve<F>(listener: TcpListener, runtime: RuntimeState, shutdown: F) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    axum::serve(listener, router(runtime))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn liveness(State(runtime): State<RuntimeState>) -> (StatusCode, Json<HealthResponse>) {
    let snapshot = runtime.snapshot();
    (StatusCode::OK, Json(health_response(&snapshot)))
}

async fn readiness(State(runtime): State<RuntimeState>) -> (StatusCode, Json<HealthResponse>) {
    let snapshot = runtime.snapshot();
    let response = health_response(&snapshot);
    let status = if response.can_prove_result {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, Json(response))
}

fn health_response(snapshot: &RuntimeSnapshot) -> HealthResponse {
    HealthResponse {
        alive: true,
        phase: snapshot.phase(),
        can_prove_result: snapshot.prove_queryable(LogSequenceNumber::ZERO).is_ok(),
        generation: snapshot.generation().get(),
        applied_lsn: snapshot.progress().applied().get(),
        published_lsn: snapshot.progress().published().get(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, Uri};
    use tower::ServiceExt;

    use super::*;
    use veyra_runtime::CannotProveReason;

    #[test]
    fn bind_address_defaults_and_validates_explicit_values() {
        assert_eq!(
            parse_bind_address(None),
            Ok(SocketAddr::from(([127, 0, 0, 1], 8080)))
        );
        assert_eq!(
            parse_bind_address(Some("0.0.0.0:9090")),
            Ok(SocketAddr::from(([0, 0, 0, 0], 9090)))
        );
        assert!(parse_bind_address(Some("not-an-address")).is_err());
    }

    #[tokio::test]
    async fn router_routes_liveness_request() {
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = Uri::from_static("/health/live");

        let result = router(bootstrap_runtime()).oneshot(request).await;
        let status = result.as_ref().map(axum::response::Response::status);

        assert_eq!(status, Ok(StatusCode::OK));
    }

    #[tokio::test]
    async fn liveness_is_ok_even_while_starting() {
        let (status, Json(response)) = liveness(State(bootstrap_runtime())).await;

        assert_eq!(status, StatusCode::OK);
        assert!(response.alive);
        assert_eq!(response.phase, ServicePhase::Starting);
        assert!(!response.can_prove_result);
    }

    #[tokio::test]
    async fn readiness_fails_closed_before_projection_publish() {
        let (status, Json(response)) = readiness(State(bootstrap_runtime())).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.can_prove_result);
        assert_eq!(response.generation, GenerationId::UNPUBLISHED.get());
    }

    #[tokio::test]
    async fn readiness_is_ok_only_for_ready_snapshot() {
        let runtime = RuntimeState::new(RuntimeSnapshot::ready(
            GenerationId::new(9),
            ProjectionProgress::at(LogSequenceNumber::new(55)),
        ));
        let (status, Json(response)) = readiness(State(runtime)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(response.can_prove_result);
        assert_eq!(response.applied_lsn, 55);
        assert_eq!(response.published_lsn, 55);
    }

    #[tokio::test]
    async fn readiness_rejects_degraded_snapshot() {
        let runtime = RuntimeState::new(RuntimeSnapshot::degraded(
            GenerationId::new(9),
            ProjectionProgress::at(LogSequenceNumber::new(55)),
            CannotProveReason::CdcGap,
        ));
        let (status, Json(response)) = readiness(State(runtime)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.phase, ServicePhase::Degraded);
        assert!(!response.can_prove_result);
    }

    #[tokio::test]
    async fn serve_completes_cleanly_after_shutdown_signal() -> io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;

        serve(listener, bootstrap_runtime(), async {}).await
    }

    #[test]
    fn health_response_exposes_generation_and_lsn() {
        let snapshot = RuntimeSnapshot::ready(
            GenerationId::new(4),
            ProjectionProgress::at(LogSequenceNumber::new(77)),
        );

        assert_eq!(
            health_response(&snapshot),
            HealthResponse {
                alive: true,
                phase: ServicePhase::Ready,
                can_prove_result: true,
                generation: 4,
                applied_lsn: 77,
                published_lsn: 77,
            }
        );
    }
}
