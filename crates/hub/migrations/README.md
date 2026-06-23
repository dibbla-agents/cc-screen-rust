# Multi-tenant migrations (proposal 0001)

Forward-only `sqlx migrate` migrations for the **multi-tenant** hub. They are
applied only when the hub runs with `--features multi-tenant` and a database URL
configured; a single-tenant install has no database and never touches this dir.

The store is pluggable (SQLite-first, Postgres addable later). Migrations are
kept portable across both — plain SQL, no backend-specific types where avoidable.

Phase 1 adds the first migration (`users`, the tenant boundary on `agents`, and
`session_version`); Phase 2 adds `device_enrollments`; Phase 4 adds
`subscriptions` / `plan_limits`. See proposal 0001 §4 for the schema and §10 for
the phase ordering.
