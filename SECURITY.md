# Security Policy

Veyra is designed around fail-closed behavior. Security issues that could make an
unproven search result appear authoritative are treated as correctness vulnerabilities.

## Report privately

Do not open a public issue for a suspected vulnerability involving corruption,
cross-tenant data exposure, malformed-segment acceptance, protocol authentication,
CDC integrity, or denial-of-service primitives. Use GitHub private vulnerability
reporting when enabled for this repository.

## Security invariants

- Veyra never exposes booking mutation methods.
- PostgreSQL remains authoritative for every booking mutation and checkout price.
- Unknown schemas, unknown rule semantics, corrupt data, invalid LSN ordering, and
  internal invariant failures must not be guessed through.
- Request work, queues, buffers, and solver search space must be bounded.
- Unsafe Rust is forbidden in normal crates. Any future justified unsafe code must be
  isolated in `veyra-unsafe`, reviewed separately, fuzzed, and Miri-tested.
- Veyra must not store passport data, identity documents, card data, or unnecessary PII.
- Release builds must have no known unaccepted dependency advisories.

## Supported versions

Before the first stable release, only the current development line receives fixes.
A formal supported-version table will be published before Veyra 1.0.
