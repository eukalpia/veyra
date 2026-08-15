#![forbid(unsafe_code)]

//! Process boundary and administrative HTTP endpoints.
//!
//! The server is intentionally separate from BEAM/NIF execution. A Veyra process crash
//! therefore cannot directly crash the Elixir VM.

use std::future::Future;
use std::io;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;
use veyra_runtime::{RuntimeSnapshot, RuntimeState, ServicePhase};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};

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
/// Milestone 0 deliberately never pretends that a query projection exists.
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
