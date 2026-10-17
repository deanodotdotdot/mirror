# Claude subscription Messages transport

Task 4 provides `providers::claude::{ClaudeMessages, ClaudeMessagesConfig}` as a
library adapter. It consumes the shared `ClaudeAuthorization` interface, never a
credential file, API key or imported token. Task 3 owns credentials; Task 6 owns
runtime provider selection, conversation binding, compaction, children, and
persistence of incomplete output. The local native worker now opts into that
execution path; default HTTP/eval constructors still reject subscription
selection. See [native execution](EXECUTION.md). The original transport tests
use synthetic fixtures; subsequent installed Mac/Linux production observations
are recorded separately in [ACCEPTANCE.md](ACCEPTANCE.md).

The constructor accepts a model, existing conversation/session ID, pinned
`ClaudeSessionBinding`, request timeout and authority. `stream_guarded` accepts
the existing request guard plus a cancellation future. Every attempt requests a
new immediate-use authorization lease, optionally identifying the one rejected
version. The authority performs freshness and the final guard/identity/generation
check under its lock. There is no await between receiving that lease and starting
the attempt. Cancellation and stream drop stop pending work; no transport task is
detached. Cancellation is checked between buffered payloads and callbacks and
before final completion, including when no further network wait is necessary.
Canonical callback names and indices are previews; only a successful
`Final` contains executable complete calls and original tool IDs.

## Frozen wire contract

Profile `mirror-phase0-omp-df4f996-v1` matches the Task 1 diagnostic and OMP
`df4f996e66a200ab7e0496114d43bdf85f32194d`. The client/SDK versions, betas,
Stainless headers, system identity, billing prefix, session/account/device
metadata and stable last-tool/last-system cache anchors are pinned. Source paths
and the complete MIT attribution are retained in `diagnostics/claude_oauth/` and
`mirror-agent/NOTICE`, which native packages include.

An OAuth-only ordered serializer computes the UTF-16 billing suffix and the
full-seed XXH64 fingerprint. It serializes the final body once and patches only
the generated five-byte CCH slot. Those exact bytes are sent. The frozen Bun
1.4.3 fixture and all five hash vectors are consumed directly by Rust tests;
ordinary tests do not run Bun. Unicode/user billing-marker text remains intact.
The API-key serializer and header profile remain independent.

Only validated `tool_choice: {type: tool, name: canonical}` and bounded
`stop_sequences` are accepted through subscription `extra_body`; other keys are
rejected before authorization. Required identity, billing, model, stream, tool,
cache and reasoning fields cannot be replaced. Forced selection omits thinking
and context management while retaining low output effort, matching the proof.
Tool names use an injective underscore prefix (canonical maximum 63 ASCII
characters); duplicates/invalid names fail without truncation. Historical tool
names can replay, but returned names must be currently exposed tools.

The explicit capability map exposes Opus 4.8 and Sonnet 4.6 with `none`/`low`
reasoning presets. Phase 0 observed signed replay for both. Sonnet 5 exposes only
`none`: its text/tools were observed, its reasoning was not. Other models and
higher effort presets remain unsupported until reviewed and fixture-covered.
The 128,000 output ceiling is observed metadata, not an entitlement claim.

## Replay and stream validation

Outgoing copies retain signed/redacted blocks only when stored provenance
matches the exact model, billing mode and profile. Unknown legacy provenance is
not portable. Normalization emits a structured notice, preserves visible text,
tool calls and results, and leaves canonical messages untouched. A turn made
empty by removing opaque reasoning is omitted only from the outgoing view.
API-key and subscription responses record provenance in existing provider fields.
API-key normalization and response provenance use the effective request model,
including the existing supported `extra_body.model` override. Normalization
works in both directions without manufacturing signatures or converting opaque
reasoning into unsigned text.

OAuth validates block indices/lifetimes (only one open block), delta types,
complete object arguments, initial input, unique IDs, thinking signatures and
terminal events around the shared Anthropic accumulator. Initial tool input and empty-object arguments are
included in canonical callbacks; fragmented JSON remains ordered by index.
Unknown tools and invalid/truncated streams never produce a successful final
completion. Mirror's current canonical message representation requires blocks
in thinking/redacted-thinking, then text, then tools order. The OAuth decoder
rejects interleavings it cannot represent without reordering signed history,
such as thinking after text/tools or text after tools. These can be valid upstream
shapes; this is a current Mirror representation/profile limitation. It returns
`partial_stream_error` after unsafe output or `compatibility_error` before any
output, with no retry, successful final completion or executable tools.
Independent review and Task 8's two-model live gate must check this boundary.
Frames are bounded at 2 MiB and a response at 32 MiB; request lifetime follows
the configured timeout or caller cancellation. Partial callbacks followed
by a typed terminal failure are persisted by native execution as incomplete,
display-only output before its terminal error event.

## Error and retry provenance

All production requests use the fixed HTTPS Messages endpoint, redirects off,
proxies off and reqwest automatic retries off. Loopback endpoints are accessible
only through private test construction. Error bodies are bounded at 64 KiB and
only allowlisted status/code/time fields escape; provider messages, arbitrary
codes, URLs and raw bodies are never included in errors.

| Evidence / response | Result |
| --- | --- |
| HTTP 401 | One fresh authorization with the rejected version; another 401 stops with `login_expired`. |
| HTTP 403 / 404 | Terminal `access_denied` / `model_unavailable`. |
| 429 `rate_limit_error` plus `error.details.error_code=enforced_spend_limit_reached` | Terminal `subscription_limited`, unknown window/scope. This is the documented API spend-cap code; it does not establish a subscription-window schema. |
| 429 plus global `anthropic-ratelimit-unified-status: rejected` | Terminal `subscription_limited`, unknown scope/reset/extra-usage setting. This header rule is observational, not an official or Phase 0-tested contract. |
| Other 429 | Terminal neutral `rate_limited`, with a valid Retry-After time when present. No automatic 429 retry. |
| 500 with `api_error`, 502/503/504, 529 with `overloaded_error`, or pre-output SSE `overloaded_error` | Eligible for the single shared transient budget. |
| Transport failure with uncertain acceptance, or any failure after text/reasoning/tool/opaque output | Terminal; no automatic replay. Partial streams have no `Final`. |

The transport permits one 401 recovery and at most two transient retries, with
at most sixty seconds of cumulative cancellable backoff. A larger provider delay
returns its safe retry time, including during auth recovery. Invalid or
unrepresentable delay hints stop recovery. Notices do not count as model output.
Every Claude error crossing into the outer agent is terminal, preventing a
second retry loop.

The spend-code distinction and Retry-After behavior come from the official
[API rate-limit documentation](https://platform.claude.com/docs/en/api/rate-limits),
checked 2026-10-17. The global rejected-header report comes from the firsthand
[Claude Code issue 29604](https://github.com/anthropics/claude-code/issues/29604).
The pinned OMP [usage parser](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/usage/claude.ts)
and its `claude-usage-headers.test.ts` establish counter/window/reset parsing,
not per-window rejection-status precedence. No such precedence, exhaustion from
utilization percentages, or extra-usage setting is inferred here. No quota
exhaustion, revocation, or billing change was induced for this task.
