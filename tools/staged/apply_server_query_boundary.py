from pathlib import Path

cargo_path = Path("crates/veyra-server/Cargo.toml")
cargo = cargo_path.read_text()
old_deps = '''tracing-subscriber.workspace = true
veyra-runtime = { version = "0.1.0", path = "../veyra-runtime" }
veyra-types = { version = "0.1.0", path = "../veyra-types" }

[dev-dependencies]
tower.workspace = true
'''
new_deps = '''tracing-subscriber.workspace = true
veyra-query = { version = "0.1.0", path = "../veyra-query" }
veyra-runtime = { version = "0.1.0", path = "../veyra-runtime" }
veyra-types = { version = "0.1.0", path = "../veyra-types" }

[dev-dependencies]
tower.workspace = true
veyra-party = { version = "0.1.0", path = "../veyra-party" }
'''
if cargo.count(old_deps) != 1:
    raise SystemExit("server Cargo dependency anchor changed")
cargo_path.write_text(cargo.replace(old_deps, new_deps, 1))

path = Path("crates/veyra-server/src/lib.rs")
text = path.read_text()
text = text.replace("use std::future::Future;\n", "use std::fmt;\nuse std::future::Future;\n", 1)
old_import = '''use tokio::net::TcpListener;
use veyra_runtime::{RuntimeSnapshot, RuntimeState, ServicePhase};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};
'''
new_import = '''use tokio::net::TcpListener;
use veyra_query::{
    MultiRoomSearchResult, MultiRoomStayQuery, QueryError, SearchEngine, SearchResult, StayQuery,
};
use veyra_runtime::{
    CannotProveReason, GenerationState, RuntimeSnapshot, RuntimeState, ServicePhase,
};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};
'''
if text.count(old_import) != 1:
    raise SystemExit("server import anchor changed")
text = text.replace(old_import, new_import, 1)

marker = '''/// Parses the optional `VEYRA_BIND` value without touching process-global state.
'''
block = '''/// Typed query boundary backed only by atomically-published immutable generations.
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
            Self::CannotProve(CannotProveReason::UnsupportedSemantics) => "unsupported_semantics",
            Self::CannotProve(CannotProveReason::Overloaded) => "overloaded",
            Self::CannotProve(CannotProveReason::InternalInvariantFailure) => {
                "internal_invariant_failure"
            }
            Self::Query(QueryError::MissingRoomTopology(_)) => "unsupported_semantics",
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

'''
if text.count(marker) != 1:
    raise SystemExit("server query block anchor changed")
path.write_text(text.replace(marker, block + marker, 1))
