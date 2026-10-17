# Phase 0 evidence

**Gate: PENDING LIVE PROOF.** No live login, token exchange/refresh, model lookup,
inference, or Claude Code coexistence operation has been performed by Task 1.
No Claude Code/OMP credentials have been read, copied, refreshed, or overwritten.
The implementation is ready for manager-coordinated review and browser sign-in.

Source baseline: `f9380337783b43804c25e5ffff60f389d311f018`.
Specification baseline: root `SPEC.md`, 2026-10-17, unchanged by this task.
Compatibility: `mirror-phase0-omp-df4f996-v1`, based on OMP
`df4f996e66a200ab7e0496114d43bdf85f32194d`.

## Synthetic verification observed on 2026-10-17

| Check | Outcome and boundary |
| --- | --- |
| macOS arm64, Python 3.14.7 | 26 unittest cases pass. Local callback tests exercise IPv4 and IPv6 when available. |
| Linux arm64, Python 3.12 (`python:3.12-slim`) | The same 26 cases pass with container networking disabled and source mounted read-only. This is not an SSH/browser login observation. |
| Independent wire oracle | Frozen synthetic request's UTF-16 billing seed and final-body CCH match Bun 1.4.3; five XXH64 vectors cover empty, small, 256-byte and complete request inputs. |
| Login/callback fixtures | PKCE, exact redirect, URL and code-plus-state validation, wrong/duplicate state/code, bare code bound to pending state, replay, five-minute timeout, occupied port fallback, hidden manual pipe path, cancellation, browser-launch failure, restored terminal echo. |
| Grant fixtures | Bearer/inference scope validation, rejection of API keys and malformed grants, authenticated bootstrap identity, no incomplete commit, newer logout wins over older login, actual-expiry calculation, refresh rotation/preservation, failed-refresh retention, generation tombstone. |
| Private storage fixtures | 0700 ancestors and dedicated directory, 0600 grant/lock, restart, atomic-write failure retention/cleanup, unknown schema, unsafe permissions and symlink rejection. |
| Messages fixtures | Three-turn streamed text/tool/result, fixed harmless tool, reversible names/forced selection/IDs, signed and redacted reasoning unchanged on replay, Unicode, cache blocks, exact serialized-body fingerprint, no patch of user `cch=00000`, override rejection, unknown tool error, unsigned/truncated stream rejection. |
| Model fixtures | Paginated lookup, authenticated request, checked bounds/cursors, absent context limits remain unknown. |
| Secret/endpoint fixtures | Redacted Secret repr, safe CLI/provider errors, no raw bodies or callback/code echo, no API-key header, proxy-env isolation, fixed origin, redirect rejection, no automatic retry. |

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
