# Claude subscription support in Mirror

Status: implementation specification; product decisions resolved with Dean.
Date: 2026-10-17.

## 1. Outcome and decisions

Use an existing Claude subscription from Mirror's own coding harness. Sign in
once in Mirror with the same Claude account, then use Mirror's tools, history,
compaction, and child agents with automatically renewed subscription access.
Claude Code and Mirror must remain usable simultaneously.

This is a direct Anthropic OAuth integration modeled on OMP. Mirror obtains
and owns a separate grant; it does not copy, borrow, refresh, or overwrite
Claude Code's or OMP's credentials. A separate login is not a new subscription.
The user explicitly accepted this design after learning that OMP's normal flow
owns its grant instead of sharing Claude Code's rotating refresh token.

### Accepted product decisions

| ID | Decision |
| --- | --- |
| D1 | A one-time browser sign-in inside Mirror is acceptable. No external credential import in v1. This supersedes the initial preference to reuse Claude Code's stored login. |
| D2 | Never fall back to a paid API key. Stop and explain recovery on auth failure or an exhausted subscription limit. |
| D3 | Use OMP's direct OAuth approach; do not use Claude Code as the inference backend. |
| D4 | Support macOS and Linux, interactive Mirror, SSH/headless authentication, and `mirror exec`. |
| D5 | One active Claude account/subscription workspace per Mirror user configuration. No account rotation. |
| D6 | Separate **Claude subscription** and **Anthropic API** selections. |
| D7 | Provide an on-demand usage command. No background quota polling or persistent quota widget. |
| D8 | Use Mirror's existing private-file convention, with atomic writes and restrictive permissions. No OS credential-store dependency. |
| D9 | Respect existing account-level extra-usage settings and display them. Do not change those settings. This is separate from the prohibition on API-key fallback. |
| D10 | Automatic renewal, following OMP's independently owned grant model. |
| D11 | Concurrent Claude Code and Mirror use must work; concurrent Mirror processes must also work. |
| D12 | Authenticate on each host. No transfer of laptop credentials to SSH hosts. With D1 resolved, login on each host is through Mirror; installing Claude Code there is unnecessary. |
| D13 | If the identity backing a Mirror conversation changes, stop before another request and require explicit reconnect. Changing Claude Code's separate login has no effect on Mirror. |

All product decisions above are closed. Technical release gates below require evidence
from implementation; they are not claims that live authentication has already
been tested while writing this document.

### Scope and deliberate differences from OMP

| Capability | v1 |
| --- | --- |
| Browser OAuth, persistent login, automatic refresh | Required |
| Direct Messages streaming, Mirror tool round trips, reasoning and replay | Required |
| Model picker and context discovery using the subscription | Required |
| Compaction and child agents using the selected subscription | Required |
| Account identity, explicit reconnect, local logout | Required |
| Usage windows, reset times and extra-usage status on demand | Required |
| Multiple accounts, automatic account rotation | Excluded |
| Importing Claude Code/OMP tokens or accepting pasted access tokens | Excluded |
| `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_OAUTH_TOKEN` onboarding | Excluded; use Mirror's saved grant |
| Arbitrary OAuth proxies/custom endpoints, auth broker, hosted gateways | Excluded |
| Windows, native Claude Code process adapter, API-key fallback | Excluded |
| Dedicated ACP onboarding, eval API expansion, standalone HTTP-server enablement | Excluded from v1 acceptance; unsupported paths must fail clearly and keep compiling |

“Parity” refers to the included authentication and subscription behavior. It
does not mean porting OMP's tools, all provider features, or all Claude-specific
preview features.

## 2. Evidence and existing architecture

Inspected on 2026-10-17:

- Mirror baseline: `f9380337783b43804c25e5ffff60f389d311f018`.
- OMP reference: `df4f996e66a200ab7e0496114d43bdf85f32194d`.
- Installed OMP: 18.11.3; installed Claude Code: 2.1.302. The OMP source
  reference is a pinned comparison, not a claim it exactly matches that binary.
- No user tokens were read and no live login/inference request was performed
  while preparing this specification.

| Existing location | Current behavior and required integration |
| --- | --- |
| `mirror-agent/src/providers/model_provider.rs` | Anthropic resolves an explicit key, `ANTHROPIC_API_KEY`, or a saved key. Add an explicit subscription provider and typed credential context. |
| `mirror-agent/src/providers/anthropic_messages.rs` | Sends `x-api-key`; already handles tool calls, thinking signatures, SSE, and caching. Reuse that machinery with an isolated OAuth compatibility layer. |
| `mirror-agent/src/providers/model_metadata.rs` | Performs independent API-key model lookup. Route OAuth metadata through the same credential authority as inference. |
| `mirror-cli/src/mirror/cli/extensions/inference_controls/extension.py` | Fetches Anthropic models in Python; bare `/login` and `/logout` concern Reflection. Add Claude commands and Rust-backed subscription metadata. |
| `mirror-cli/src/mirror/cli/cli.py` | Anthropic onboarding collects an API key. Add subscription login/progress/cancellation without placing secrets in chat. |
| `mirror-cli/src/mirror/cli/models.py` | Model selections retain model, API, and endpoint. Persist subscription mode and conversation identity binding without tokens. |
| `mirror-agent/src/main.rs` | Constructs the provider and optional compaction provider for the Python-launched worker. Carry OAuth selection through both. |
| `mirror-agent/src/tools/subagent.rs` | Derives child providers from active inference configuration. Preserve the parent's auth/identity binding and give children distinct session IDs. |
| `mirror-agent/src/react/agent.rs` | Currently treats model errors broadly as retryable. Terminal OAuth errors and partial-stream failures need explicit retry semantics. |
| `mirror-agent/src/http/cli.rs` | Supplies `OPENAI_API_KEY` for non-Reflection providers. An excluded OAuth invocation must be rejected before this key can be used. |

Mirror currently saves Reflection and Anthropic API keys in private files under
`~/.mirror/`, using a temporary file and atomic replacement. Its Codex adapter
reads and updates the Codex-owned login file. Neither behavior establishes
Claude OAuth support; the new Claude grant will be Mirror-owned.

OMP's normal OAuth flow stores its own grant in `~/.omp/agent/agent.db` and
coordinates renewal within that store. Mirror should match the ownership and
renewal guarantees, not adopt OMP's database or general account-broker design.

## 3. User-facing contract

### Commands

```text
mirror auth login claude [--no-browser]
mirror auth status claude [--json]
mirror auth logout claude
mirror usage claude [--json]
mirror --api anthropic-oauth --model <model-id>
mirror exec --api anthropic-oauth --model <model-id> "<task>"
```

Interactive equivalents:

```text
/login claude
/logout claude
/usage claude
/reconnect claude
/model anthropic-oauth
```

- Add `ModelApi::AnthropicOAuth`, with the public value `anthropic-oauth` and
  picker label **Claude subscription**. Keep `anthropic-messages` and the
  existing `anthropic` Rust alias as API-key access, labeled **Anthropic API**.
- Bare `/login` and `/logout` retain their Reflection behavior. Their help lists
  the explicit Claude form; arguments must no longer be silently ignored.
- Login establishes/replaces credentials without switching the active model.
  Selecting **Claude subscription** with no login offers login, then returns
  to the requested model choice. Escape restores the previous selection and
  preserves the user's draft. Login never runs a paid completion as a probe.
- A provider switch is committed only after validating local configuration and
  the usable login. Normal startup resolution and saved-model precedence stay
  intact. A new login does not make Claude the global default implicitly.
- The provider default initially follows Mirror's Anthropic default model. A
  user-selected model always wins. Unavailable models produce an actionable
  error/model picker; never silently replace the selected model.
- `mirror auth status claude` reports local state without renewing or probing
  the network. Status includes the account/workspace, source **Mirror login**,
  expiry and recovery action. Locally refreshable is distinct from logged out.
- `/reconnect claude` explicitly binds the current idle conversation to the
  current Mirror account/workspace and displays that identity. It performs no
  inference. Commands that change a conversation's binding require it to be
  idle; there is no mid-request account switch.
- Logout clears Mirror's grant and advances its generation. It does not invoke
  Claude Code logout, alter OMP, revoke another app's grant, or erase history.
  Requests already authorized for dispatch may finish; later authorization
  attempts must stop, as defined in section 7.
- Authentication/usage commands run before chat creation, extension loading,
  tools, and workspace trust. The Python parser must reserve `auth` and `usage`
  explicitly instead of treating them as free-text exec prompts. Users can
  still send those words as prompts using `mirror exec "auth ..."`.

### Billing and environment precedence

`anthropic-oauth` resolves only Mirror's saved Claude grant. It never consults
API-key variables/files, bearer-token variables, `apiKeyHelper`, Claude Code
storage, OMP storage, or cloud-provider credentials. Supplying `--api-key`
with this provider is a configuration error. Existing API-key sessions remain
API-key sessions even after login.

Reject explicit nondefault `--base-url`/model-entry endpoints for subscription
mode. Reject a conflicting `ANTHROPIC_BASE_URL` with a clear instruction to
unset it or select the API provider; do not silently reroute or reinterpret the
session. Do not inherit `MIRROR_CODEX_AUTH_PATH` as a Claude credential path.
No implicit credential or provider change is permitted for inference, model
lookup, usage, child agents, compaction, retry, or resume.

Account-level extra usage can be billed on the same subscription grant. Honor
the account's existing setting, show the reported setting in `/usage claude`,
and do not claim that absence of API-key fallback guarantees zero extra costs.
Do not enable extra usage, purchase credits, redeem reset credits, or mutate
account billing preferences.

## 4. Component boundaries

Implement one Rust `ClaudeAuthManager` (name illustrative) responsible for
loading/storing the grant, renewal, identity, request authorization, and safe
errors. Inference and metadata use it directly. Add a local `mirror-agent auth`
command/API path alongside the existing memory/sandbox command paths so Python
can use the same implementation before a conversation/worker exists.

Python owns the UI and opening the browser. It receives a narrow structured
progress/status stream, never access or refresh tokens. Submit manual callback
input over private stdin/pipes, not argv, environment, tool calls, or chat.
The Rust command owns pending state, PKCE, callback listener and exchange.

Separate these concepts:

- **Credential generation:** changes on login replacement/logout; prevents
  stale refresh work from overwriting newer credentials.
- **Token revision:** changes on successful renewal within the same generation.
- **Identity binding:** account UUID plus subscription organization/workspace
  UUID, persisted with the conversation and inherited by all its model work.
- **Request compatibility profile:** pinned wire conventions and fixture
  version, independent of the user's credentials and model selection.

No raw secret belongs in `ModelProviderConfig` debug output, protobufs, model
entries, workspace preferences, traces, metrics, exception text, or chat DBs.
Use typed access-token/refresh-token values with redacted debug formatting.

## 5. Browser and headless login

The pinned OMP implementation uses these upstream endpoints:

| Purpose | Endpoint |
| --- | --- |
| Subscription authorization | `https://claude.ai/oauth/authorize` |
| Code exchange and refresh | `https://api.anthropic.com/v1/oauth/token` |
| Account/workspace bootstrap when needed | `https://api.anthropic.com/api/claude_cli/bootstrap` |
| Inference | `https://api.anthropic.com/v1/messages` |
| Models | `https://api.anthropic.com/v1/models` |
| Usage | `https://api.anthropic.com/api/oauth/usage` |

Keep OAuth client identity, scopes, redirect rules and exchange/refresh headers
in one reviewed compatibility configuration, sourced from the pinned OMP flow.
The client ID is public protocol metadata. Never create an Anthropic API key as
part of login, even if the upstream scope set includes that capability.

Login sequence:

1. Create a random state and PKCE verifier, derive an S256 challenge, and record
   the current credential generation. Keep these values only in memory.
2. Start a loopback callback listener. Match OMP's preferred `localhost:54545`
   and `/callback`, binding only loopback addresses and handling IPv4/IPv6
   localhost resolution. If selecting a free fallback port, put that exact
   redirect URI in both authorization and token exchange. Validate provider
   acceptance of the fallback in the initial live spike.
3. Emit the authorization URL to the dedicated login UI/terminal. Open it unless
   `--no-browser` is set. A browser-launch failure leaves manual completion
   available rather than destroying the flow.
4. Accept either the loopback callback or a manually pasted final callback
   URL/provider-issued code. Validate every supplied state against the pending
   state, including the provider's code-plus-state format. A code-only response
   must still use the pending state/verifier; do not accept unrelated callbacks.
5. Exchange the code once. Validate token types, nonempty tokens, positive
   `expires_in`, and subscription inference scope. Reject Console/API-key
   credentials as the wrong authentication kind.
6. Obtain account and organization identity from the exchange or authenticated
   bootstrap. Do not infer identity from an unrelated config file or email.
   Commit a replacement login only once identity is known; an incomplete login
   reports failure and leaves any existing working login intact.
7. Acquire the credential lock and compare the starting generation. A newer
   login/logout wins; an older pending browser flow cannot overwrite it.
   Atomically commit the complete grant, then report success and close the
   listener. A failed disk write is not a successful login.

Expire pending flows after five minutes, close listeners on cancellation or
process exit, and reject replayed callbacks. Keep callback payloads small and
never fetch a pasted URL. No arbitrary redirect/authorization server override.
Do not echo authorization codes or include callback queries in logs.

On SSH/Linux, run `mirror auth login claude --no-browser` on that host and open
the displayed URL in a browser elsewhere. Paste the result into that host's
login prompt. No Claude Code installation or laptop credential transfer is
needed. `mirror exec` and other noninteractive model requests never open a
browser or consume task stdin for login; they fail with the login command.

## 6. Grant storage and renewal

Use the private-file backend selected in D8, preserving the lifecycle and
concurrency guarantees below.

Use `~/.mirror/claude_oauth.json` and a separate stable lock file in `~/.mirror/`.
The credentials never appear in `models.json`, the conversation database, or
an OMP/Claude Code file. No new secrets database is required for one login.

Versioned record, showing fields rather than real credential values:

```json
{
  "schema_version": 1,
  "generation": "<random UUID>",
  "revision": 1,
  "installation_id": "<Mirror-generated UUID>",
  "credential": {
    "access_token": "<secret>",
    "refresh_token": "<secret>",
    "expires_at": "<UTC timestamp>",
    "authorized_at": "<UTC timestamp>",
    "account_id": "<account UUID>",
    "organization_id": "<subscription workspace UUID>",
    "email": "<optional display label>",
    "organization_name": "<optional display label>",
    "scopes": ["<granted scope>"]
  }
}
```

Logout replaces `credential` with `null` and changes `generation`, leaving a
nonsecret tombstone so in-flight renewal cannot restore the deleted grant.
Login replacement also changes generation; renewal only increments revision.
Reject unknown schema versions with an upgrade/recovery error, without deletion.

Create new directories with mode `0700` and secret/lock files with mode `0600`.
Do not change permissions on unrelated preexisting files. Reject an unexpected
owner or symlink substitution. Write a temporary file in the same directory,
flush it, atomically rename it, and flush the directory as appropriate. The
stable lock must not be the inode that atomic replacement swaps out.

### Renewal algorithm

1. Resolve and check the conversation identity, cancellation/request guard,
   and credential generation before each authenticated operation.
2. Use actual server expiry. Refresh when fewer than five minutes remain,
   storing the real expiry so skew is not subtracted repeatedly. Opaque tokens
   must not be treated as JWTs.
3. Use a cancellable cross-process lock, then reload. If another process
   already renewed the token, use its revision. Hold the lock across the
   bounded exchange and commit so one refresh token is not rotated twice.
4. Bound token HTTP exchange to ten seconds and lock waiting to thirty seconds.
   Use the pinned refresh request profile. A missing replacement refresh token
   preserves the old one; malformed/empty replacement values fail validation.
5. Preserve account/workspace identity and original login time on refresh.
   If the server reports a different identity, stop; do not silently rebind.
6. Recheck generation/revision and the existing record before atomic commit.
   Publish the new token to callers only after persistence succeeds.
7. On an initial unauthorized inference response, reload first. If the rejected
   token is still current, coordinate at most one forced refresh, then retry
   only before response content/tool effects. Do not repeatedly remint a token
   during an outage; reuse a recently minted revision for concurrent recovery.

Network/5xx/temporary throttling failures retain credentials and report a
retryable recovery condition. `invalid_grant`, revoked credentials, or missing
refresh material require login. Reload once before treating an older rejected
refresh token as definitive, in case another operation committed a newer login.
No browser flow starts automatically from renewal.

Timeout or process death after the provider rotated a token but before local
commit is an ambiguous outcome. Preserve the last record and report recovery;
if the grant is no longer usable, require login. Do not claim exactly-once
server-side renewal or guaranteed recovery from this crash window.

Two Mirror processes use the same lock. Claude Code/OMP use independently
issued grants and different storage; Mirror never takes their locks or writes
their records. Live coexistence testing must verify signing into/refreshing
Mirror does not invalidate those other apps' grants.

## 7. Identity binding, resume, and logout

Bind the account/workspace when a conversation first selects the subscription
provider. Persist only that identity and provider selection, not tokens or
authorization URLs. Every subsequent dispatch resolves the current grant and
checks the binding, including metadata, retries, child agents and compaction.

Replacing a grant for the same identity can proceed transparently. A different
account/workspace produces `account_changed` before transmitting conversation
content. Display the old/new labels and the `/reconnect claude` action. No
automatic organization change, even when the email address is identical.
A resumed conversation follows the same rule. A new exec conversation binds
the current identity; exec has no interactive reconnect prompt.

Immediately before each network attempt, authorize dispatch by checking the
current generation and identity under the credential lock. This check orders
the request against login/logout; release the lock before network I/O. Do not
reuse that authorization across a wait or retry. Logout prevents subsequent
authorizations, while a request authorized before logout may finish even if
its network activity overlaps logout. Do not retry a failed request using an
in-memory token after logout.
Changing Claude Code's independent login neither rebinds nor logs out Mirror.

## 8. OAuth Messages compatibility

Keep the normal API-key request path stable. The OAuth path uses bearer
credentials and an isolated compatibility adapter. The pinned OMP adapter is
an interoperability reference, not an official stable third-party API contract.

The adapter must account for Claude Code-style client/beta headers, system
identity/billing blocks, session/account metadata, wire tool-name mapping,
cache placement, and final serialized-body fingerprinting. Do not stop after
replacing `x-api-key` with a bearer header. Record the exact adopted conventions
and source revision in test fixtures; keep API-key requests free of these
subscription-only transformations.

Serialize once after applying caller options and compatibility constraints;
any body fingerprint must cover the exact bytes sent. Never patch a matching
string in user content. Reserve transport-controlled fields so `extra_body`
cannot replace authentication identity, required system blocks, session IDs,
or force another billing mode. Reject conflicting overrides before dispatch.
Keep user instructions intact after transport-only prefixes and keep those
prefixes out of persisted canonical conversation messages.

Use a stable Mirror installation ID and conversation session ID, with child
sessions distinct from the parent. Do not read another app's device identity.
Maintain deterministic cache boundaries across ordinary turns and renewals.
Pin an active session's compatibility profile; no silent per-turn changes in
identity or serialization caused by guessed version upgrades.

Tool names remain canonical internally. Use an explicit reversible mapping on
definitions, assistant history, forced tool selection, and received tool
calls. Check name lengths and collisions, including existing prefixes and
extension-qualified names. Preserve tool-call IDs through results and replay.
Unknown returned tool names are errors, not permission to execute new tools.

Preserve signed thinking/redacted blocks on compatible replay. Model switches
and cross-provider history must follow a tested normalization rule: preserve
visible text and tool results, omit only incompatible provider-specific opaque
reasoning with a clear notice, and never manufacture or alter signatures.
The same rule must handle resuming API-key history through subscription access
and the reverse, instead of assuming signatures are portable.

For models exposed in the picker, validate tool support, context/output limits,
and reasoning capabilities. Translate Mirror's effort levels only through an
explicit model capability map; do not send unsupported `xhigh`, `max`, adaptive
thinking, or sampling settings blindly. Unknown capabilities remain unknown.
Do not advertise untested preview features as part of authentication parity.

All credential-bearing requests use reviewed HTTPS Anthropic origins and
redirects disabled. Keep authorization, refresh, inference, model, and usage
header profiles explicit. Recheck the request guard before network dispatch
and after a wait/retry. Synthetic test transports may use loopback; production
configuration cannot redirect a real grant to an arbitrary endpoint.

## 9. Model discovery, compaction, and usage

The Rust authority supplies subscription model discovery to Python, instead
of Python reading the credential file. Handle pagination, a ten-second metadata
budget and bounded payloads. Cache by provider plus account/workspace identity
and invalidate on identity change/logout. A temporary discovery outage may use
last-known results or a bundled catalog with a stale/offline label. Such a
catalog lists candidates, not proven entitlements; missing login is never
concealed by an offline catalog.

Per-model context discovery uses the same authority. Explicit context settings
retain current precedence; validated metadata is preferred otherwise, then a
known model fallback. Retain the current conservative Anthropic fallback for
unknown models and make it visible; do not borrow another model's limit.
Do not infer one-million-token access from a model name alone.

A child agent inherits the active provider and identity binding. A compaction
request inherits them unless the user explicitly configured another provider.
An intentionally configured API compaction model remains valid, but must be
reported as such; it is not an implicit quota/auth fallback. Provider switches
clear provider-specific credential references. The existing Codex `auth_path`
must not accidentally survive into the Claude provider.

`mirror usage claude` and `/usage claude` make an on-demand request, using the
same renewable grant. Show provider-reported shared/model-scoped windows,
percentages, reset times, extra-usage enabled/disabled/unknown, and any reported
spend/limit with units. Handle legacy named windows and generic scoped limits.
Missing data means unavailable, not zero usage or unlimited access. Show fetch
time and the user's local timezone; JSON uses UTC timestamps.

Coalesce concurrent usage requests and reuse a result for up to thirty seconds.
Honor 429 delay hints and return stale data with its age when appropriate.
No timer-based polling. Usage failure does not invalidate a working inference
grant. Quota percentages are informational, since other sessions can consume
the same subscription; inference responses determine whether a request was
accepted. Never switch accounts/models/providers to work around a limit.

## 10. Errors, retries, and machine-readable output

Use stable error kinds shared by provider, worker and CLI:

| Kind | Behavior |
| --- | --- |
| `login_required` / `login_expired` | Stop; give `mirror auth login claude` and preserve the conversation. |
| `login_cancelled` / `login_timeout` | Preserve old credentials and model selection; no partial commit. |
| `credential_storage_error` | Report storage recovery; no alternate secret destination or false login success. |
| `account_changed` | Stop before content transmission; require explicit reconnect. |
| `subscription_limited` | Stop; show the actual reported scope/reset/extra-usage status, if present. |
| `model_unavailable` / `access_denied` | Explain the selected model/account; do not misdiagnose token expiry. |
| `auth_temporarily_unavailable` | Retain credentials; bounded recovery, then stop with retry guidance. |
| `compatibility_error` | Show safe provider error and compatibility revision; no automatic version spoofing guesses. |
| `invalid_configuration` | Stop before resolving/transmitting secrets. |
| `partial_stream_error` | Preserve partial output, mark it incomplete, and do not replay automatically. |

Do not classify errors solely by matching a raw error string. Preserve safe
HTTP status, provider error code and reset/retry metadata in typed results.
The API's usage-limit response may be distinct from short-term throttling;
classify from validated provider details rather than assuming every 429 means
subscription exhaustion.

Before any streamed output, permit at most one auth recovery and up to two
retries for explicitly transient responses, with a shared per-request retry
budget and a maximum sixty-second backoff budget. Retry delays are cancellable;
if a provider delay exceeds the budget, return the retry time. Do not retry a
request of uncertain acceptance after a transport failure without a verified
safe rule. No automatic retry after text, reasoning, a tool-call fragment, or
other replay-unsafe output is exposed. A subsequent outer-agent loop must
respect these terminal decisions instead of retrying again.

Do not count an auth retry as a new tool invocation, execute tools from an
incomplete response, or report a failure as successful task completion when
an exception count is reached. Preserve existing exec failure conventions and
nonzero exits; do not write auth prompts to exec/ACP stdout.

`--json` status/usage output has a schema version, provider, state/identity,
collection timestamp where relevant, and structured error/recovery fields.
It never includes secret tokens, codes, verifier, authorization URLs, or raw
upstream response bodies. Redaction also applies to Debug, stderr, traces,
parse errors, and network errors. Account labels may be shown to the user but
are not included in generic telemetry.

## 11. Implementation sequence

### Phase 0: prove the external contract

Before building full onboarding, use synthetic fixture tests and a controlled
live spike to prove a fresh Mirror-owned grant, renewal, model lookup, and a
multi-turn Mirror tool round trip. Verify localhost and headless callback flows,
model-specific reasoning, body compatibility, and simultaneous Claude Code use.
Record the source/compatibility revisions and supported model/OS observations.
A missing required endpoint capability is a design blocker: ask the user before
substituting shared credentials or the official Claude Code backend.

### Phase 1: credential authority and commands

Implement the Rust auth component, chosen storage backend, lock/transaction
semantics, status, login/logout, typed errors and the local command transport.
Add synthetic tests for refresh races, cancellation, identity and secret
handling before connecting the live agent.

### Phase 2: provider and all model consumers

Add `AnthropicOAuth`, shared Messages transport and compatibility module.
Integrate model metadata, child agents, compaction, runtime provider switches,
and conversation identity binding. Extend `LlmError` and outer React retry
handling. Preserve existing API-key behavior and provider guards.

### Phase 3: UI and persistence

Update the Python launcher/parser, actions, login UI, model picker, preferences,
reconnect and on-demand usage. Add additive nonsecret fields to
`protocols/mirror_agent/protos/agent.proto` and session preferences only where
needed; preserve existing field numbers and regenerate bindings. Older saved
sessions retain their existing billing mode; missing OAuth identity must be
explicitly established before their first subscription request.

### Phase 4: acceptance and documentation

Run focused Rust/provider and Python CLI tests, then required repository checks
(`cargo test --workspace` and `bash ci.sh`, plus native build checks required by
the repository's CI). Check that the packaging includes new native command and
platform dependencies. No implementation tests are being run for this Markdown
change. Update CLI README/configuration/help with login, SSH setup, storage,
usage, failure recovery and the deliberate OMP scope differences.

Expected files include the existing locations in section 2, plus
`mirror-agent/src/providers/llm.rs`, `mirror-cli/src/mirror/cli/main.py`,
`mirror-cli/src/mirror/main.py`, `mirror-cli/src/mirror/cli/actions.py`,
`mirror-cli/src/mirror/cli/workspace_controller.py`, and new Rust auth/compat
modules. Update all exhaustive `ModelApi` matches and all config constructors,
including the eval Python binding, without expanding its public API by default.

Pin compatibility constants with source provenance and fixture coverage.
Upstream changes require a reviewed update, not a runtime download of OMP code
or an automatic change based on a server's version string. Preserve applicable
license/attribution when adapting upstream code.

## 12. Acceptance criteria

Each item is a release gate, not just a suggested test:

- **A1 — Fresh login:** an account with no Anthropic API key signs in once,
  selects Claude subscription, performs text and read/edit/command tool turns,
  and remains authenticated after restarting Mirror.
- **A2 — Headless:** Linux over SSH authenticates through a browser on another
  machine and runs `mirror exec` without interactive prompts during the task.
  Browser-open failure, occupied callback port, cancellation, wrong/replayed
  state, and timeout are covered with deterministic tests.
- **A3 — Billing isolation:** environment/saved API keys coexist with the login.
  Auth/quota failures never send a fallback API-key request, including in
  metadata, compaction and child agents. Existing account extra usage is
  respected and correctly described by the usage command.
- **A4 — Renewal:** a near-expiry grant renews, persists atomically and works
  after restart. Two independent Mirror processes racing renewal converge on
  one replacement. Test logout/login races, process death, malformed responses,
  disk errors, ambiguous network timeout and revoked grants.
- **A5 — Coexistence:** Claude Code and Mirror both complete requests before and
  after Mirror login/refresh/logout; their credential stores remain independent.
  A Claude Code account switch does not change Mirror's account.
- **A6 — Identity:** a new Mirror account/workspace stops existing bound
  conversations before further requests. `/reconnect claude` resumes only after
  an explicit action. Test resume, child work, retries and compaction boundaries.
- **A7 — Wire behavior:** golden fixtures cover tool naming and history replay,
  reasoning signatures, Unicode prompts, cache blocks, final serialized-body
  transformations, conflicting extra-body overrides and stream truncation.
  No duplicate tool execution or repeated partial output on failure.
- **A8 — Models:** paginated discovery and context lookup authenticate correctly.
  Catalog outage uses a labeled fallback; revoked login does not. At least the
  shipped default model and another available Claude model pass tool/reasoning
  checks. All advertised capability presets have fixture coverage.
- **A9 — Usage:** shared/model-specific windows, null/unknown data, stale data,
  timezone formatting, extra usage and 429 backoff are correct. No background
  polling and no inference failure just because usage reporting is unavailable.
- **A10 — Secrets/endpoints:** synthetic canary secrets never reach chat/history,
  logs/Debug, argv, preferences or JSON status output. A custom host, redirect,
  or provider switch never receives the subscription token.
- **A11 — Compatibility:** existing Reflection, Anthropic API and Codex flows,
  provider switching, saved selections, exec output and packaging keep working.
  Excluded launch surfaces produce a clear unsupported/configuration error.

Live acceptance must record Mirror revision, compatibility revision, platform,
model IDs and outcomes without secrets. Use a real consenting account for the
small happy-path/coexistence checks and genuine renewal validation; never
force quota exhaustion or revocation just to test errors. Model/quota/revoked
cases use synthetic fixtures. Simulated expiry alone is not proof of real
server-side refresh interoperability.

## References

- [OMP provider/login behavior](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/docs/providers.md).
- [OMP OAuth configuration](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/catalog/src/compat/rules/auth/anthropic.kdl).
- [OMP callback engine](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/registry/engine/oauth-code.ts) and [callback listener](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/registry/oauth/callback-server.ts).
- [OMP refresh coordination](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/auth/refresh.ts) and [refresh-race tests](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/test/auth-storage-oauth-refresh-race.test.ts).
- [OMP Anthropic transport](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/providers/anthropic.ts), [client identity profile](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/providers/claude-code-fingerprint.ts), and [identity mapping](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/providers/anthropic-identity.ts).
- [OMP usage parsing](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/usage/claude.ts) and [account API headers/endpoints](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/usage/claude-api.ts).
- [Anthropic authentication documentation](https://code.claude.com/docs/en/authentication) and [credential-use guidance](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use), checked 2026-10-17. Technical interoperability is distinct from official support; this spec does not claim Anthropic endorsement.
