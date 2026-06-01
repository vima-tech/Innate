# Repository Guidelines

## Project Structure & Module Organization

Innate is a Python 3.10+ knowledge-layer library. Keep changes within the existing three-layer architecture:

- `innate/core/`: SDK logic, SQLite access, embeddings, refinement, and exceptions. Only this layer may read or write the database.
- `innate/cli/`: thin Click adapter mapping commands to the core API.
- `innate/daemon/`: runtime log watcher; invoke the CLI instead of accessing SQLite directly.
- `migrations/`: base `schema.sql` and ordered schema upgrades.
- `tests/`: pytest coverage grouped by behavior, boundaries, CLI, and design compliance.
- `docs/Innate-设计文档-v4.5.1.md`: authoritative implementation baseline.
- `skills/innate-memory/`: installable agent skill metadata.

## Build, Test, and Development Commands

```bash
pip install -e ".[dev]"            # editable install with test dependencies
python -m pytest tests/             # run the full suite
python -m pytest tests/test_cli.py -q
python -m compileall -q innate tests
python -m innate --help             # exercise the package entry point
innate inspect                      # inspect the default knowledge database
```

Use `INNATE_DB=/tmp/example.db` or `innate --db /tmp/example.db ...` for isolated CLI checks.

## Coding Style & Naming Conventions

Follow the existing Python style: four-space indentation, type hints for public interfaces, short docstrings for behavioral contracts, and `snake_case` names for modules, functions, and tests. Use `PascalCase` for classes. No formatter or linter is configured, so keep edits focused and readable. Use `utc_now_iso()` for Python timestamps; do not introduce ad hoc time formatting.

## Testing Guidelines

Tests use pytest. Name files `test_<area>.py` and cases `test_<behavior>()`. Add regression tests for every contract change, especially recall tracing, dependency closure, Curate lifecycle rules, schema migration, and CLI/daemon behavior. Run the full suite before submitting.

## Commit & Pull Request Guidelines

History is small, but new commits should use concise imperative subjects, preferably Conventional Commit prefixes such as `feat:`, `fix:`, `test:`, or `docs:`. Keep commits scoped. Pull requests should state the behavior changed, reference the relevant design section, list verification commands, and call out schema or CLI compatibility effects.

## Architecture Notes

Treat the design document as the source of truth. Preserve the downward dependency rule: daemon → CLI → core. Update migrations, docs, and regression tests together when a persisted contract changes.
