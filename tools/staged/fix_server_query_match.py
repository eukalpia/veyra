from pathlib import Path

path = Path("crates/veyra-server/src/lib.rs")
text = path.read_text()
old = '''            Self::CannotProve(CannotProveReason::UnsupportedSemantics) => "unsupported_semantics",
            Self::CannotProve(CannotProveReason::Overloaded) => "overloaded",
            Self::CannotProve(CannotProveReason::InternalInvariantFailure) => {
                "internal_invariant_failure"
            }
            Self::Query(QueryError::MissingRoomTopology(_)) => "unsupported_semantics",
            Self::Query(_) => "query_rejected",
'''
new = '''            Self::CannotProve(CannotProveReason::UnsupportedSemantics)
            | Self::Query(QueryError::MissingRoomTopology(_)) => "unsupported_semantics",
            Self::CannotProve(CannotProveReason::Overloaded) => "overloaded",
            Self::CannotProve(CannotProveReason::InternalInvariantFailure) => {
                "internal_invariant_failure"
            }
            Self::Query(_) => "query_rejected",
'''
if text.count(old) != 1:
    raise SystemExit("service query error match anchor changed")
path.write_text(text.replace(old, new, 1))
