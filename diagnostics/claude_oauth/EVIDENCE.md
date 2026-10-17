# Phase 0 evidence

**Gate: PENDING LIVE PROOF.** No diagnostic login, token exchange/refresh, model
lookup, inference, or complete Claude Code coexistence sequence has been performed.
No Claude Code/OMP credentials have been read, copied, refreshed, or overwritten.
The diagnostic has completed Astra review/fixes and is ready for the manager
to coordinate private browser sign-in. Review does not pass the live gate.

Source baseline: `f9380337783b43804c25e5ffff60f389d311f018`.
Specification baseline: root `SPEC.md`, 2026-10-17, unchanged by this task.
Compatibility: `mirror-phase0-omp-df4f996-v1`, based on OMP
`df4f996e66a200ab7e0496114d43bdf85f32194d`.

## Synthetic verification observed on 2026-10-17

| Check | Outcome and boundary |
| --- | --- |
| macOS arm64, Python 3.14.7 | 35 unittest cases pass after Astra review. Local callback tests exercise IPv4 and IPv6 when available. |
| Linux arm64, Python 3.12.15 | The same 35 cases pass as nonroot `mirror-test` with source mounted read-only. Unmocked HTTP/browser launches are blocked by the tests. This is not an SSH/browser login observation. The original 26-case suite also passed with Docker networking disabled. |
| Independent wire oracle | Frozen synthetic request's UTF-16 billing seed and final-body CCH match Bun 1.4.3; five XXH64 vectors cover empty, small, 256-byte and complete request inputs. |
| Login/callback fixtures | PKCE, exact redirect, URL and code-plus-state validation, wrong/duplicate state/code, bare code bound to pending state, replay, five-minute timeout, occupied port fallback, hidden manual pipe path, cancellation, browser-launch failure, restored terminal echo. |
| Grant fixtures | Bearer/inference scope validation, rejection of API keys and malformed grants, authenticated bootstrap identity, no incomplete commit, newer logout wins over older login, actual-expiry calculation, refresh rotation/preservation, failed-refresh retention, generation tombstone. |
| Private storage fixtures | 0700 ancestors and dedicated directory, 0600 grant/lock, restart, atomic-write failure retention/cleanup, unknown schema, unsafe permissions and symlink rejection. |
| Messages fixtures | Three-turn streamed text/tool/result, fixed harmless tool, reversible names/forced selection/IDs, signed and redacted reasoning unchanged on replay, Unicode, cache blocks, exact serialized-body fingerprint, no patch of user `cch=00000`, override rejection, unknown tool error, unsigned/truncated stream rejection. |
| Model fixtures | Paginated lookup, authenticated request, checked bounds/cursors, absent context limits remain unknown. |
| Secret/endpoint fixtures | Redacted Secret repr, safe CLI/provider errors, no raw bodies or callback/code echo, no API-key header, proxy-env isolation, fixed origin, redirect rejection, no automatic retry. |

## Astra review and fixes, 2026-10-17

No P0/P1 findings. P2 findings corrected before first live use:

- Callback and streamed `readline` loops could stay alive indefinitely under
  slow byte trickles. Callbacks now have an absolute connection budget; stream
  reads check bounded chunks; a main-thread POSIX timer covers HTTP headers and
  bodies. Regression tests also verify timer cleanup.
- Storage checked a pathname before opening it and permitted unsafe writable
  ancestors. It now walks `O_NOFOLLOW` directory descriptors, validates the
  opened owner/mode, and rejects unsafe ancestors. A separate-process test
  verifies the stable lock survives atomic grant replacement. Oversized writes
  cannot replace a record that must remain reloadable.
- Forced-tool fixtures kept adaptive thinking and clear-thinking fields,
  contradicting pinned OMP's `disableThinkingIfToolChoiceForced`. Those fields
  are omitted for forced selection; low effort remains. Bun 1.4.3 independently
  revalidated the regenerated request hash.
- Opaque reasoning incorrectly inherited the 8 KiB credential-field limit.
  Signed/redacted content now remains exact within the bounded SSE payload,
  including a 16 KiB regression fixture for each kind.
- A different model returned by the detail endpoint could be reported as a
  successful lookup. Mismatched IDs now stop before inference.

Malformed Unicode callback states now reject without terminating login, and
safe CLI failures carry fixed recovery instructions. The pinned source/MIT
notice, endpoint/header profiles, PKCE, tool mapping, cache placement and exact
serialized-byte fingerprint were compared directly; no newer client version
was adopted. No production code or accepted specification decision changed.

Reproduce with `python3 -m unittest discover -s diagnostics/claude_oauth/tests -v`.
These are synthetic tests, including tests whose mocked successful refresh result
says `real_token_endpoint_refresh`: no mocked result is live evidence. Normal
test runs do not invoke Bun or make external network requests.

## Live evidence still required

Use the sequence in [README.md](README.md#coordinated-live-run). The helper emits
sanitized JSON with Mirror revision, compatibility revision, platform, model,
collection time and checks. Commit only those observations and concise outcomes,
never tokens, token hashes, codes, callback URLs, authorization URLs or bodies.

| Required observation | Status | Revision / platform / model / time / outcome |
| --- | --- | --- |
| Fresh separate Mirror-owned subscription grant, identity and restart | Pending | — |
| Preferred `localhost:54545` browser callback | Pending | — |
| Actual random-port redirect accepted upstream | Pending | — |
| Hidden manual callback on a local terminal | Pending | — |
| Linux/SSH login with browser on another machine | Pending | — |
| Real server refresh, persisted replacement, successful use after restart | Pending | — |
| Subscription model list and model-detail/context fields | Pending | — |
| Mirror default `claude-opus-4-8`: streamed text/tool/result and signed reasoning replay | Pending | — |
| Second discovered/reviewed model: streamed text/tool/result and signed reasoning replay | Pending | — |
| Claude Code succeeds before/after Mirror login/refresh/local logout | Pending | — |
| Simultaneous Claude Code request and Mirror probe both succeed | Pending | — |

No missing capability is presumed supported. A server/profile/endpoint failure
must be reported as a blocker before full production integration. Do not modify
billing settings, force exhaustion, or revoke a real grant to test failures.

The spike cannot establish A1–A11 end-to-end acceptance. It deliberately omits
production Rust authority, automatic renewal and dispatch races, full Mirror
read/edit/command tools, compaction, child agents, UI, usage parsing, cross-model
normalization, packaging and billing/identity acceptance. Those remain assigned
to the later tasks in [STATUS.md](../../docs/claude-integration/STATUS.md).

Manager-reported pre-change baseline (not rerun as proof of this diagnostic):
525 Rust agent library tests passed, with six jj-dependent failures resolved by
the pinned jj tool; all 15 checkpoint tests then passed. Three agent binary
tests, protocol tests and Python binding cargo check passed. Python baseline had
3075 passes/5 skips/9 rendering failures under `TERM=dumb,NO_COLOR=1`; all ten
affected parametrizations passed with `NO_COLOR` unset and `TERM=xterm-256color`.
