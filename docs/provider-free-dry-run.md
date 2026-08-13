# Provider-free acceptance record

This document is the operator record for `make provider-free-acceptance`. It
does not authorize a paid campaign and must never point at a live factory
database or `../xsh`.

The target requires four distinct externally created PostgreSQL 18 databases,
each named `factory_test_v3_<digits>`, plus a quiescent source runtime/database
pair and an empty restore database/runtime root for backup verification:

```sh
FACTORY_ACCEPTANCE_POSTGRES_URL='postgresql://USER@localhost/factory_test_v3_101' \
FACTORY_ACCEPTANCE_DECISION_URL='postgresql://USER@localhost/factory_test_v3_102' \
FACTORY_ACCEPTANCE_XSH_BUNDLE_URL='postgresql://USER@localhost/factory_test_v3_103' \
FACTORY_ACCEPTANCE_VERTICAL_URL='postgresql://USER@localhost/factory_test_v3_104' \
FACTORY_BACKUP_SOURCE_DATABASE_URL='postgresql://USER@localhost/factory_v3_live' \
FACTORY_BACKUP_SOURCE_RUNTIME_ROOT=/absolute/path/to/live-runtime \
FACTORY_BACKUP_RESTORE_DATABASE_URL='postgresql://USER@localhost/factory_restore_v3_105' \
FACTORY_BACKUP_RESTORE_RUNTIME_ROOT=/absolute/path/to/empty-restore-runtime \
FACTORY_BACKUP_DUMP_FILE=/absolute/path/to/retained/factory-v3.backup \
FACTORY_BACKUP_PG_DUMP="$(command -v pg_dump)" \
FACTORY_BACKUP_PG_RESTORE="$(command -v pg_restore)" \
FACTORY_BACKUP_PSQL="$(command -v psql)" \
FACTORY_BACKUP_CARGO="$(command -v cargo)" \
make provider-free-acceptance
```

The acceptance sequence verifies local source/host checks, serial PostgreSQL
authority, SQLx metadata, exact XSH bundle admission, a generic Product →
Engineering → Quality → delivery composition, and backup/restore integrity.
It uses scripted actors and synthetic repositories where appropriate. It never
spends provider budget, starts a live campaign, or pushes Git.

Keep the backup dump and source CAS/database pair as operator evidence. The
restore check copies only append-only CAS objects into its isolated root; it
does not copy sockets, locks, staging files, or worktrees. See
[testing](TESTING.md) for scope and [evidence](EVIDENCE.md) for retention.
