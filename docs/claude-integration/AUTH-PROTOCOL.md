# Private Claude native command protocol, version 1

`mirror-agent auth --stdio` runs one operation before worker configuration,
workspace access, tools or ordinary worker logging. Python launches it using
private FIFO stdin/stdout pipes and consumes the events directly. Never send
these pipes to `LocalRpcClient` log collection. Native code never opens a browser
or terminal; Python owns browser launch, `--no-browser`, hidden input and terminal
restoration. In particular, the actual Darwin controlling `/dev/tty` requires
Task 7 coverage; a pipe or slave PTY alone does not reproduce that terminal.

Write one initial JSON line and keep stdin open until the terminal event:

```json
{"schema_version":1,"request_id":"11111111-1111-4111-8111-111111111111","type":"request","provider":"anthropic-oauth","action":"login","params":{}}
```

Auth actions are `login`, `status` and `logout`, each with exactly empty params.
`models`, `model_context` and `usage` are reserved for Task 5's additive typed
actions/results. They currently fail explicitly as not implemented. Each
request ID is a nonnil UUID. Fields, provider, schema and action are validated;
unknown fields are rejected. No secrets are permitted in arguments/environment.

During login, submit callback input or cancel on private stdin:

```json
{"schema_version":1,"request_id":"11111111-1111-4111-8111-111111111111","type":"callback","value":"<final callback URL or provider-issued code>"}
{"schema_version":1,"request_id":"11111111-1111-4111-8111-111111111111","type":"cancel"}
```

Input lines are at most 16 KiB before newline; decoded callback input is at most
8192 UTF-8 bytes. The initial request has a five-second framing deadline. Pending
login, including exchange/bootstrap/commit waiting, expires after five minutes.
Wrong callback state/format yields a fixed rejection notice and keeps the pending
flow; valid input consumes it once, including if exchange subsequently fails.
No pasted URL is fetched. Only fixed reviewed upstream endpoints are used.

Every output has `schema_version:1`, the validated `request_id`, fixed
`provider:"anthropic-oauth"`, native-owned
`compatibility_profile:profile::PROFILE_ID`, the action and one of these forms:

```typescript
{ type: "progress", phase: Phase, notice: null | "callback_rejected" }
{ type: "authorization_url", authorization_url: string, expires_at: Rfc3339Utc }
{ type: "completed", result: { status: ClaudeAuthStatus } }
{ type: "error", error: ClaudeError, phase: Phase | "protocol",
  commit_outcome: null | "not_committed" | "unknown" }
```

`Phase` is `preparing`, `awaiting_callback`, `exchanging_code`,
`resolving_identity` or `committing`. Shared status/error contracts are defined in
`mirror-agent/src/providers/claude/contracts.rs`. `status` and `logout` emit only
one terminal completed/error event. Login can also emit progress and its single
ephemeral URL. Failure to validate initial input permits null request ID/action;
unvalidated input is never echoed. Output lines are bounded at 4 MiB for future
normalized metadata. Writes have a one-second budget and require no blocking
stdin/stdout reader threads.

Only the transient `authorization_url` event and callback input may contain the
flow's raw URL/code. Do not log, persist, trace, debug or place either in chat.
Status, errors and all other events contain no token, code, verifier, URL, raw
provider body, arbitrary error text or callback fragment. UI messages must come
from fixed error/phase/recovery enums. Account display labels are user-facing and
excluded from generic telemetry. Public `mirror auth status claude --json`
prints the contained `result.status` plus `compatibility_profile` from the
common envelope, rather than the full private transport envelope.

EOF, cancel, SIGINT and SIGTERM cancel a pending operation. Python must keep stdin
open; `communicate(input)` closes it too early. Cancellation drops pending
network futures and loopback listeners without detached renewal or reads.
Before a mutation's synchronous commit boundary it reports `not_committed`.
Already-buffered cancel/EOF and ready signals are handled before polling an
uncommitted operation. Login/logout recheck cancellation immediately before
starting the synchronous write, including when exchange/bootstrap or a lock
becomes ready in the same poll.
Once a durable login/logout commit finishes, completed wins later cancellation.
Native code selects exactly one terminal result, writes it and exits. A received
terminal event determines the outcome; subsequent exit/cancellation races must
not reinterpret completed as rollback. Exit codes are 0 for completed, 130 for
cancellation and 1 for other errors. A local status query succeeds with 0 even
when its state is logged_out/login_required.

Python restores terminal state immediately on cancellation, sends cancel, allows
two seconds for native cleanup, then SIGTERM for one second and SIGKILL/reap.
Native output writes are also bounded. EOF without a delivered terminal result
means interrupted/unknown; inspect status before retrying a mutation. A failed
directory flush after rename explicitly reports `commit_outcome:"unknown"`.
Never roll back a possibly rotated grant or claim the old record survived.

## Local authority and metadata cache ordering

`ClaudeAuthManager` implements `ClaudeAuthorization` from Task 2.
`local_snapshot(cancellation)` returns nonserializable
`ClaudeLocalAuthSnapshot { status, version }` under the credential lock. It does
not mutate storage, renew, probe or expose tokens. Missing storage gives no
version; logout retains a generation with revision zero. Known authoritative
refresh revocation is persisted as a validated generation/revision-scoped
`reauth_required` marker, retaining the grant but reporting login_required.
Endpoint errors from usage/models/inference never set that marker.

Metadata may hold its separate lock while taking a local snapshot or requesting
authorization. The sole nested order is **metadata -> authority**. Login/logout
never wait for a metadata lock; credential generation invalidates old cache
entries. First-bind/reconnect can accept usable/refreshable plus identity and
the frozen profile ID; Task 6 owns conversation storage and explicit reconnect.

Each network attempt calls `authorize` immediately before dispatch. No lease
survives a wait/retry. Lock wait is cancellable and bounded at 30 seconds; refresh
HTTP is bounded at ten seconds and held under the stable credential lock through
commit. Authorization revalidates the stable lock and stored record after the
final asynchronous request guard, and checks cancellation again before returning
a lease. A completed renewal stays persisted even when that request is cancelled.
Successful refresh preserves original authorized_at, increments revision
and records actual expiry plus last_refreshed_at. A rejected current revision
minted within 60 seconds reports temporary auth recovery; a different persisted
revision is reloaded first. A server rotation followed by timeout/process death
before commit is ambiguous and may require a new login. There is no force-refresh
CLI, imported credential source, endpoint override or API-key fallback.
