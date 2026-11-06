---
name: engineering-standards
description: Team standards for code design, commits, PRs, testing, and deployment. Use when designing code, preparing a PR, or planning tests or a deploy.
user-invocable: false
---

# Engineering Standards

Team engineering standards for code design, commits, pull requests, testing, and deployment. RFC 2119 keywords (MUST, SHOULD, etc.) carry their standard meanings.

## Code Design

### Principles

- Every change applies SOLID, DRY, KISS, and YAGNI. One responsibility per function, class, or module.
- Build only what the change requires now. No speculative hooks, flags, config, or generality.
- Factor out duplication once it genuinely recurs. Never couple unrelated code that only looks alike.
- Prefer composition over inheritance. Inherit only for a real is-a relationship, kept shallow.
- Depend on an existing service interface rather than a concretion where one already covers the capability.
- Names state what a thing is or does, not how or when. Match the conventions of the surrounding code over a personal preference.

### Boundaries and interfaces

- A module's dependencies MUST point one way. No import cycles between modules.
- Untrusted or untyped input MUST be validated at the boundary where it enters, before any use. Boundaries include HTTP requests, environment variables and config, external API responses, queue and event payloads, and anything parsed off disk. For JS/TS, `playbook:engineering-standards-javascript` names the schema-validator pattern. For Rust, `playbook:engineering-standards-rust` names the parse-into-a-newtype pattern. For Python, `playbook:engineering-standards-python` names the parse-into-a-model pattern.
- A change to a published interface (an API response, a database schema, an event payload, an exported function) MUST be additive first: add the new shape, migrate the readers, remove the old shape in a later change. Never break a consumer and its producer in the same deploy.
- Model a multi-step process as a named sequence of steps, not implicit control flow spread across helpers.

### Constants and configuration

- No magic values. A number or string a reader would have to look up, or that appears in more than one place, MUST be named: a constant near its use, or configuration when it varies by environment or gets tuned. This covers thresholds, limits, timeouts, retry counts, sizes, status codes, feature keys, and repeated identifiers.
- The name MUST state what the value means, not repeat the value: `MAX_RETRIES`, not `THREE`.
- Exempt: 0, 1, and -1 as identity or sentinel values; loop and index arithmetic; a literal at its single definition site (a log message, a route, a SQL fragment, one map key); expected values and fixture data in tests.
- This applies to code you write or change. Extracting constants from untouched neighbouring code is a separate change, not a drive-by.
- Secrets MUST NOT be hardcoded, committed, or logged. They come from configuration or a secret store.

### Errors and observability

- An error MUST NOT be silently swallowed. Handle it, log it with context, or let it propagate to a layer that does.
- Error messages state exactly what failed and why, in plain language. They MUST NOT carry PII or PHI; reference a record by a non-sensitive identifier instead.
- Service and API operations SHOULD be idempotent and safely retryable wherever the operation allows. Retries SHOULD use backoff.
- Log at boundaries and at each failure point of a multi-step process, so a failure's location is visible from the logs alone. Logs MUST NOT contain secrets, PII, or PHI.

## Commits

### Self-review before committing

`/playbook:commit-and-push` runs in a forked context on the `git` agent, so it never judges the change. Read `git diff --cached` first and ask the two things no hook can: does it belong in one commit (if the message needs an "and", split it), and can you say why in one line. `playbook hook precommit-check` already warns on secret-shaped filenames, debug leftovers and oversized commits.

## Pull Requests

### Readiness

A PR MUST meet these criteria before requesting review:

- CI is fully passing.
- Automated tests are included for the change.
- The author has self-reviewed the diff.
- The description explains the "why", not just the "what".
- Any intentional technical debt is documented in the description with justification.

### Size

- **Soft limit (500):** PRs SHOULD be under 500 changed lines (additions + deletions).
- **Enforced limit (1000):** a PR over 1000 changed lines MUST carry explicit justification; without it, split before requesting review.
- **Hard limit (1500):** PRs MUST NOT exceed 1500 changed lines. There is no override; split the work.
- Large changes SHOULD be split into logical units (e.g., one PR for the data layer, another for the service layer).
- One concern per PR. A refactor, a feature, and its docs are separate PRs, not one. Unrelated changes in a single diff force the reviewer to track several things at once.
- Treat the diff as an interface the reviewer reads (adapted from Krug's "Don't Make Me Think"): the smaller and more focused it is, the less they have to figure out. When work is large, ship a sequence of small PRs.

### Review Comments

- Code review comments SHOULD use Conventional Comments format: `<label> [decorations]: <subject>`, for example `issue (blocking): ...`. The definition is in the `playbook:grounding-review` skill, `references/conventional-comments.md`.
- Blocking comments are for issues that MUST be resolved before merge: quality gaps, undocumented tech debt, security or data integrity concerns, missing tests, or architectural issues affecting future maintenance.
- Blocking comments SHOULD be treated as opportunities for discussion, not hard stops. Valid resolutions: fix immediately, create a follow-up ticket, document the limitation, agree the concern is out of scope, or escalate to a design discussion.
- Non-blocking feedback SHOULD be framed as suggestions: "one option here..." or "worth considering...".
- Reviewers SHOULD prioritise re-reviews and PRs closest to completion over new PRs (pull work to the right).

### Review Turnaround

- Initial reviews SHOULD be completed within 24 hours.
- Re-reviews (after author addresses feedback) SHOULD be completed within 4 hours.

## Automated Testing

### Test Types

| Type | What it tests | External dependencies |
|---|---|---|
| Unit tests | Business logic of a specific function or class | None |
| Integration tests | Service and data layer behaviour | Database |
| API/container tests | API behaviour with real database, stubbed externals | API + database |
| E2E tests | Full user flows | Full environment |

### Requirements

- All code changes MUST include appropriate automated tests.
- Unit tests SHOULD focus on error handling and edge cases that are difficult to exercise in higher-level tests.
- Integration tests MUST be isolated: each test creates its own data and MUST NOT rely on shared seed data.
- Code SHOULD be structured so it could achieve high unit test coverage without requiring refactoring, even if 100% coverage is not required.
- When implementing new functionality, TDD (red/green/refactor) SHOULD be used: write a failing test first, then minimal implementation to pass, then refactor. This prevents tests that are tautologically coupled to the implementation.
- Coverage thresholds MUST NOT be decreased. If changes increase coverage, thresholds SHOULD be raised.
- CI MUST pass before a PR is eligible for review.

### Mocking

- Mock only where necessary, as close to the application boundary as possible.
- Prefer dependency injection over global mocking for easier-to-understand tests.
- For domain services: spy on the interface and verify call parameters; do NOT mock the database tables owned by another service.
- Inject an external client as a constructor or function parameter rather than constructing it inside the function under test. A function that builds its own client can't be mocked without reaching into its internals.
- Prefer a narrow, per-operation interface (one method per operation) over one generic call that branches on an argument. A mock over a narrow interface has a fixed shape, and a test visibly declares which operations it touches.

## Manual Testing

- Changes MUST be manually verified before merge, as a complement to automated testing.
- If a scenario can be covered by an automated test, it SHOULD be.
- Database migration PRs: the query SHOULD be verified by converting it to a SELECT and running it against production data first.

## Incremental Delivery

- Work MUST be delivered incrementally, not in a single large release.
- Feature flags MUST be used (where available) to enable safe, incremental rollout.
- Each work unit SHOULD be independently deliverable and testable.

## Deployment

- Deployment path: merge to main -> automated post-merge checks -> manual approval -> deploy.
- Engineers MUST monitor their changes after deployment for errors, performance regressions, and unexpected behaviour.
