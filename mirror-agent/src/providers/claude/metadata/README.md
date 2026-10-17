# Claude metadata service

`ClaudeMetadataService` owns subscription catalog, context and usage lookup. It
accepts an `Arc<dyn ClaudeAuthorization>`; production passes `ClaudeAuthManager`.
Python uses the private native actions documented in
[AUTH-PROTOCOL.md](../../../../../docs/claude-integration/AUTH-PROTOCOL.md), and
never reads a credential file or issues bearer HTTP. Runtime provider/session
binding and Python UI wiring remain Tasks 6 and 7.

## Rust seam

The public module is `providers::claude::metadata`:

- `new(authority: Arc<dyn ClaudeAuthorization>) -> Result<Self, ClaudeError>`
- `models(expected_identity: Option<&ClaudeIdentityBinding>, request_guard: Option<&dyn ModelRequestGuard>, cancellation: BoxFuture<'_, ()>) -> Result<ClaudeModelsResult, ClaudeError>`
- `model_context(expected_identity: &ClaudeIdentityBinding, model: &str, explicit_context_window: Option<u64>, request_guard: Option<&dyn ModelRequestGuard>, cancellation: BoxFuture<'_, ()>) -> Result<ClaudeModelContextResult, ClaudeError>`
- `usage(expected_identity: Option<&ClaudeIdentityBinding>, request_guard: Option<&dyn ModelRequestGuard>, cancellation: BoxFuture<'_, ()>) -> Result<ClaudeUsageResult, ClaudeError>`

Methods other than the constructor are async. Each result exposes `.info` with
common status fields and the payload fields documented by the protocol. Serde
flattens `.info` into the public JSON object. The common event envelope supplies
the Rust-owned compatibility profile.

## Endpoint and context evidence

Fixed reviewed origin: `https://api.anthropic.com`. Catalog uses `GET /v1/models`
with `limit=100` and `after_id`. Selected detail uses `GET /v1/models/{model}`;
usage uses only `GET /api/oauth/usage`. Redirects, system proxies and reqwest
retries are disabled. Conflicting `ANTHROPIC_BASE_URL` fails even on cache hits.
Neither API-key nor bearer environment variables are consulted. Loopback
transport injection exists only under `cfg(test)`.

The headers match the proven Phase 0 metadata profile in
`diagnostics/claude_oauth/compatibility.py`: OAuth beta, frozen CLI user agent,
bearer authorization and, for models, `anthropic-version: 2023-06-01`.
Selected detail is established by `proof.py:379` and its exact-ID validation
fixture; `EVIDENCE.md` records successful detail fields on macOS and Linux/SSH.
The [official list schema](https://platform.claude.com/docs/en/api/models/list)
establishes `data`, `has_more`, `last_id` and cursor pagination; OAuth access is
a Phase 0 observation, not an official third-party subscription guarantee.

Catalogs are capped at 16 pages, 1,024 distinct models and 2 MiB of total response
bytes across attempts. Repeated IDs, missing/nonadvancing cursors and mismatched
last IDs fail. Model IDs are bounded at 128 bytes; display labels at 256 bytes.
Unknown response fields are dropped before caching or output. Response errors
carry only closed error kinds/provider codes, HTTP status and parsed retry time.

Context precedence is validated explicit value, exact selected-model metadata,
reviewed exact-ID fallback, then visible conservative 200,000. Cached catalog
context is used when present; otherwise selected detail is requested. Stale
exact-model metadata may survive a temporary outage, with original collection
time and a stale/error label. `max_input_tokens` is preferred over existing
Mirror context aliases; output-token fields never supply context. The only
bundled candidates and known offline context entries are the reviewed
`claude-opus-4-8` and `claude-sonnet-4-6`, both conservatively 200,000. Names never
infer 1M. Phase 0 observed 1M metadata but did not prove long-context entitlement.
A different returned detail ID is not borrowed. Selected-model 404/403 stops;
there is no model substitution.

## Cache and authority ordering

`~/.mirror/claude_metadata.json` contains only normalized data and typed errors.
It has a separate stable `claude_metadata.lock`. Both files are private 0600,
with descriptor-relative no-symlink/type/owner/single-link checks and atomic
same-directory writes followed by file/directory flushes. Newly created `.mirror`
is 0700; existing safe owned directories keep their modes. The narrow filesystem
primitives are shared with the credential store without exposing grant records.
A failed cache write is a storage failure, never claimed successful persistence.

The cache key is provider, account, organization, credential generation and
compatibility profile. A different key invalidates all entries. Token revision
is deliberately not the cache key: renewal preserves account metadata. Catalog
and selected details are reused up to 300 seconds; successful usage up to 30.
Only 64 selected detail entries/failures are retained, with bounded eviction.
Cache bytes are capped at 2 MiB. No raw provider payloads or credentials persist.

The only nested order is **metadata -> authority**. A local snapshot is checked
before creating the metadata store, after waiting for its lock, and after work
before cache publication/return. Snapshot reads are local/read-only and contain
no tokens or renewal. Login/logout never acquire the metadata lock. Reconnect
needs no cache mutation: identity/generation/profile fencing suffices. The
request guard is verified before cache return and passed into every fresh
HTTP authorization. The stable metadata lock is revalidated after the final
asynchronous authority/guard await.

A fresh authorization lease is obtained immediately before every HTTP attempt,
including every catalog page and the single coordinated 401 recovery. Leases
are never carried across a retry/wait. The ten-second outer deadline includes
all locks, renewals, pages and reads; it does not reset per attempt. HTTP shares
an earlier nine-second absolute deadline, reserving the last second for final
local auth/guard validation and labeled stale/offline publication after a stalled
response. If authority/guard validation cannot finish inside ten seconds, the
result remains a hard error. Auth/lock timeouts never become catalog fallback.
Timeout, cancel, EOF, signals or dropping the call drop the complete pending future,
including its held metadata lock. No refresh/HTTP task is detached. As in the
authority, already-started synchronous filesystem work finishes before returning;
completed auth rotations are not rolled back after cancellation.

Missing/revoked/mismatched auth is a terminal error even with stale data.
Authorization/recovery errors are never catalog/context fallback success.
Final model 401/403 failures block catalog and selected-model fallback across
processes until a successful request or credential generation change. The
marker belongs only to metadata; it never changes the grant's revocation state.
Usage errors likewise affect only usage caching, never inference credentials.

## Usage source mapping and fixtures

The schema interpretation is pinned to OMP
[df4f996e66a200ab7e0496114d43bdf85f32194d usage/claude.ts](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/usage/claude.ts)
and [claude-api.ts](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/usage/claude-api.ts).
The synthetic fixtures contain no live account data. Relevant MIT attribution
is retained in `mirror-agent/NOTICE`.

| Input | Normalized interpretation / fixture |
| --- | --- |
| `five_hour`, `seven_day` | Shared 5-hour / 7-day windows; `utilization` is reported percentage, `resets_at` normalized UTC. `usage-legacy.json` retains a null window instead of inventing zero. |
| `seven_day_opus`, `seven_day_sonnet` | Model family windows. Nullable bucket remains reported/unknown. Percentages above 100 are preserved rather than clamped. |
| `limits[].kind` of `session`, `weekly_all`, `weekly_scoped` | Shared 5-hour, shared weekly, model weekly. Generic `percent`, `resets_at`, `scope.model.display_name` and `is_active` are preserved. `usage-generic.json` includes inactive shared/model rows and an unknown kind. |
| Unknown generic kind | Unknown duration/scope unless a model scope is reported; no guessed window. Generic IDs are source-qualified and include the array index. Legacy and generic rows are never summed. |
| `extra_usage.is_enabled` | Enabled/disabled/unknown independently of amounts. Reported amounts remain visible even when disabled. |
| `extra_usage.used_credits`, `monthly_limit` | Raw `credits`, retaining only reported `currency` and `decimal_places`. No default USD or scale is invented. Legacy fixture uses EUR scale 3 and null limit. |
| `spend.enabled`, `used/limit.amount_minor` | Raw `currency_minor` amounts, each with its own reported `currency` and `exponent` as decimal places. Generic fixture deliberately uses differing EUR and JPY scales. `spend` takes precedence when present. |
| Missing/null/invalid amount or unknown explicit unit | Unavailable amount (`null`); an invalid/missing currency or scale stays null. Null limit never means zero or unlimited. |

At most 128 windows are exposed. Nonnegative finite numeric values (including
bounded numeric strings used by OMP) are retained, with invalid values null.
No percentages, model scopes, extra-usage state or money values ever gate
inference. There is no account/model/provider rotation, quota polling, billing
change, reset-credit redemption, profile fetch or other auxiliary usage request.

A 429 is not retried. Valid numeric or HTTP-date `Retry-After` is honored;
timestamps beyond UTC year 9999 are rejected. Absent or invalid hints use a
60-second usage cooldown. Other usage fetch failures also coalesce
for 60 seconds; catalog/detail temporary failures coalesce for five seconds.
Cooldown responses preserve the original successful `fetched_at` and age. Without
prior usage data they are `unavailable`. Known auth errors stay terminal; this
informational endpoint never marks the inference grant revoked. All time output
is UTC; local timezone and human currency formatting belong to Python.
