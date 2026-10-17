# Phase 0 evidence

**Phase 0 external gate: OBSERVED / PASSED; pending manager final review and merge.**
The macOS sequence succeeded at reviewed
revision `a56e13c97e6be98308cd2d0596816167ec09b119`: fresh separate login,
restart, models, real persisted refresh, two models with signed reasoning replay,
and Claude Code coexistence before/after/concurrent with the diagnostic. A third
model passed text/tools without observed signed reasoning. Linux/SSH also passed
two signed reasoning/tool probes, real persisted refresh, concurrent Claude Code
use and local logout. The user's sanitized Linux login result directly confirms
manual completion with an upstream-accepted random callback port.

The diagnostic never read, copied, refreshed, or overwrote Claude Code/OMP
credential stores. The original review and terminal follow-up Astra review/fixes
are complete. These external-contract observations do not establish production
Mirror acceptance criteria A1–A11.

Source baseline: `f9380337783b43804c25e5ffff60f389d311f018`.
Specification baseline: root `SPEC.md`, 2026-10-17, unchanged by this task.
Compatibility: `mirror-phase0-omp-df4f996-v1`, based on OMP
`df4f996e66a200ab7e0496114d43bdf85f32194d`.

## Synthetic verification observed on 2026-10-17

| Check | Outcome and boundary |
| --- | --- |
| macOS arm64, Python 3.14.7 | 43 unittest cases pass after the terminal correction. Local callback tests exercise IPv4 and IPv6 when available; five tests run the complete login path with an actual controlling `/dev/tty`. |
| Linux arm64, Python 3.12.15 | The same 43 cases pass as nonroot UID 65534 with networking disabled and source mounted read-only. Unmocked HTTP/browser launches are also blocked by the tests. This is not an SSH/browser login observation. |
| Linux/SSH manual terminal entry | A separate manager-owned `ssh -tt` PTY at a56e13c completed hidden code-plus-Enter input with exit 0, manual callback mode, one mock token exchange, a temporary grant, restored terminal state and redacted public output. Transport and the authorization URL were synthetic; no browser or provider request occurred. This verifies actual SSH terminal input, not live headless OAuth. |
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
  cannot replace a record that must remain reloadable. Directory-flush failure
  after replacement explicitly reports uncertain durability without rolling
  back a potentially rotated token.
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

## First live attempt and terminal correction, 2026-10-17

At reviewed revision `d261caa97e571001fd419db8c55a5fea107fac05`, the user opened
the login browser flow in a private macOS terminal. The browser redirected to the
preferred localhost callback, but the helper exited with only `diagnostic_failed`.
No grant was saved and no process/listener remained. This is a failed attempt,
not proof of token exchange or callback completion; no callback payload is
retained or repeated in this document.

A fully synthetic reproduction identified the local defect: Darwin's
`KqueueSelector` rejects registration of an actual controlling `/dev/tty` with
`EINVAL` (errno 22), while accepting a slave PTY and pipes. Earlier tests mocked
the terminal's wait function or used pipes, and therefore missed that distinction.
`SelectSelector` accepts the same controlling terminal. The manager independently
confirmed this without invoking a live login.

Callback waiting now uses `SelectSelector` for its small fixed descriptor set.
Five subprocess tests create a real controlling terminal with `pty.fork()` and
exercise loopback login, hidden manual completion after browser-launch failure,
Ctrl-C cancellation, a synthetic token-exchange exception, and a cleanup flush
failure. They use synthetic
browser callbacks and token responses only, verify terminal echo restoration and
listener cleanup, and prevent token/code/identity leakage to public output. The
real transport's JSON reader is exercised against a mocked opener; no provider
request or existing grant is accessed.

Failures now add allowlisted `stage` and `error_class` labels. Unexpected errors
retain only a fixed class category, with no exception name, message, values,
cause, traceback, URLs, tokens, callback codes, or identities. Stage-label tests
also reject arbitrary labels and preserve the innermost failure stage. This
correction changes no endpoint, scope, OAuth exchange, or compatibility profile.

The follow-up Astra review fixed one P2 cleanup edge: a failed terminal input
flush skipped echo restoration. Cleanup now attempts restoration independently,
with a real controlling-terminal regression and safe terminal recovery guidance.
The cancellation test permits delivery during either the announcement/browser
stage or callback wait, avoiding a scheduler-dependent stage assertion. All 43
tests passed on macOS and nonroot Linux with networking disabled in Docker. No
P0/P1 or outstanding P2 findings remain in this follow-up; no live login was
performed by the reviewer.

Reproduce with `python3 -m unittest discover -s diagnostics/claude_oauth/tests -v`.
These are synthetic tests, including tests whose mocked successful refresh result
says `real_token_endpoint_refresh`: no mocked result is live evidence. Normal
test runs do not invoke Bun or make external network requests.

## Controlled live observations, 2026-10-17

The manager coordinated the consenting user's private browser login and small
diagnostic/Claude Code requests. This ledger was transcribed only from sanitized
JSON observations under `/tmp/mirror-claude-manager/live/` and the four explicit
before/after/concurrent observation files under `/tmp/mirror-claude-coexistence/`,
the additional `after-linux-login-refresh.json` observation there, plus the
manager's report of the user's Linux cancellation confirmation.
No grant, authorization/callback URL, code, token hash, raw provider body or
account identity is reproduced here. All times below are UTC on 2026-10-17.

Successful diagnostic revision: `a56e13c97e6be98308cd2d0596816167ec09b119`.
Profile: `mirror-phase0-omp-df4f996-v1`; platform: macOS (`darwin`).

| Required observation | Status | Revision / platform / model / time / outcome |
| --- | --- | --- |
| Fresh separate Mirror-owned subscription grant, identity and restart | Observed on macOS | Login succeeded at 19:17:13.776956Z; loopback/preferred callback. A separate helper process reported usable state, revision 1. Successful commit requires validated account/workspace identity. |
| Preferred `localhost:54545` browser callback | Observed on macOS | Fresh login above completed through the preferred loopback callback. This supersedes the earlier failed local attempt described above. |
| Actual random-port redirect accepted upstream | Observed on Linux/SSH | At 19:26:09.796450Z, the user's sanitized successful login result reported fresh_mirror_owned_grant=true, callback_mode=manual and callback_port=random. |
| Hidden manual callback input for the headless flow | Observed on Linux/SSH | The successful Linux flow above completed by manual submission in the waiting hidden SSH prompt, using a browser on another machine. macOS manual input remains synthetic controlling-terminal coverage; no additional macOS manual login is required by this gate. |
| Linux/SSH login with browser on another machine | Observed | Linux established its own fresh grant at 19:26:09.796450Z, expiry 2026-10-18T03:26:09.790990Z. A separate SSH helper process reported usable revision 1, followed by the successful Linux sequence below. |
| Real server refresh, persisted replacement, successful use after restart | Observed on macOS | At 19:18:04.859980Z, the real token endpoint returned success and a replacement refresh token; the helper persisted revision 2 with expiry 2026-10-18T03:18:04.823645Z. Subsequent separate-process Sonnet 5 and Sonnet 4.6 probes succeeded. No expiry simulation was used. |
| Subscription model list and model-detail/context fields | Observed on macOS | At 19:17:25.392092Z, discovery exposed 14 models including all three reviewed candidates. Their observed detail responses reported max_input_tokens=1000000 and max_tokens=128000. Long-context inference was not tested. |
| Mirror default `claude-opus-4-8`: streamed text/tool/result and signed reasoning replay | Observed on macOS | Probe ran 19:17:44.632342Z–19:17:49.155902Z, exit 0. Text and fixed echo tool/result passed over three requests; one signed reasoning block was replayed. |
| Second discovered/reviewed model: streamed text/tool/result and signed reasoning replay | Observed on macOS | `claude-sonnet-4-6` ran 19:18:39.567107Z–19:18:44.070058Z after refresh, exit 0. Text and tool/result passed over three requests; two signed reasoning blocks were replayed. |
| Additional model: `claude-sonnet-5` | Text/tools observed; reasoning unverified | Ran 19:18:14.931557Z–19:18:18.046627Z after refresh, exit 0. Text/tool round trip passed over three requests, but zero signed reasoning blocks were observed. This model does not independently satisfy the reasoning gate. |
| Claude Code succeeds before/after Mirror login/refresh/local logout | Observed on macOS | Claude Code used `claude-opus-5-5`; each exact-OK request exited 0. Before: 19:01:39.956717Z–19:01:45.531622Z. After login: 19:17:44.630179Z–19:17:47.262520Z. After refresh: 19:18:14.930257Z–19:18:17.273211Z. After local logout: 19:19:01.655892Z–19:19:05.397054Z. |
| Simultaneous Claude Code request and Mirror probe both succeed | Observed on macOS | The after-login Claude Code request overlapped the successful Opus 4.8 diagnostic probe above. The after-refresh Claude Code request also overlapped the successful Sonnet 5 probe. |
| Diagnostic local logout and independent status | Observed on macOS | Local logout reported success at 19:19:01.537780Z; an independent helper status returned logged_out (exit 0), collected at 19:19:23.279875Z. Claude Code's subsequent request still succeeded. |

### Linux/SSH sequence

The remote host was Linux aarch64, Python 3.12.15, UID 1000. Mounted `proof.py`
and `compatibility.py` matched clean reviewed revision
`a56e13c97e6be98308cd2d0596816167ec09b119` by SHA256. Remote envelopes report
`mirror_revision: unavailable` because the worktree's Git path is host-specific;
the independent source comparison supplies their revision provenance:

| Source | Observed matching SHA256 |
| --- | --- |
| `diagnostics/claude_oauth/proof.py` | `9381262c0a94bdc595aa1b972d8f1f165f756bcb92e82c373fd45096b57e85d5` |
| `diagnostics/claude_oauth/compatibility.py` | `b92f32a6482bc8ba3c2f51e222d88f842c8d7e77a1b9f8af93a3df510490e120` |

| Observation | Outcome |
| --- | --- |
| Separate-host login and restart | The user's sanitized login result at 19:26:09.796450Z reports fresh_mirror_owned_grant=true, callback_mode=manual, callback_port=random and expiry 2026-10-18T03:26:09.790990Z. The manager then observed usable revision 1 in a separate SSH helper process. No laptop grant was transferred. |
| `claude-opus-4-8` before refresh | 19:27:43.256785Z–19:27:48.271711Z, exit 0. Model lookup, text and fixed echo tool/result passed over three requests; one signed reasoning block was replayed. |
| Real refresh and persisted replacement | At 19:28:01.757934Z, exit 0. The token endpoint returned success and a replacement refresh token; revision 2 persisted, with expiry 2026-10-18T03:28:01.750948Z. |
| `claude-sonnet-4-6` after refresh | A new SSH helper process ran 19:28:08.614561Z–19:28:13.550516Z, exit 0. Model lookup, text and tool/result passed over three requests; two signed reasoning blocks were replayed. Both Linux model details reported max_input_tokens=1000000 and max_tokens=128000. |
| Concurrent Claude Code | `claude-opus-5-5` produced exact OK, exit 0, during 19:28:08.615822Z–19:28:11.257229Z, overlapping the Linux Sonnet 4.6 probe after real refresh. |
| Local logout and independent status | Logout succeeded at 19:28:40.392841Z. A separate status command returned logged_out at 19:28:40.617342Z, both exit 0. |

An earlier Linux attempt returned `login_cancelled` / `callback_wait` /
`cancelled` with no grant. The user confirmed reaching the localhost callback,
then pressing Ctrl-C before submitting it to the SSH prompt. This was expected
cancellation; it is not evidence of a new helper defect. A separate synthetic
test then verified the actual SSH hidden-input-plus-Enter path without provider
calls. The recorded successful manual/random-port Linux login and subsequent
probes supersede that cancelled attempt.

Discovery metadata is evidence of the reported fields, not proof of a one-million
token entitlement or successful long-context inference. The live tool was only
the diagnostic's fixed echo. It did not exercise Mirror's read/edit/command
tools. The refresh observation proves a real endpoint exchange and successful
persisted use; it does not prove production automatic renewal, cross-process
refresh races, or crash-window recovery. Coexistence establishes that this
separate diagnostic grant's login/refresh/local logout left the existing Claude
Code session usable; account switching and full application acceptance remain
later work.

The run sequence is in [README.md](README.md#coordinated-live-run). Required
Phase 0 external-contract observations are now recorded: fresh independent
grants, preferred localhost and Linux/SSH manual/random callbacks, real persisted
refresh, discovery, two-model text/tool/signed reasoning replay, and simultaneous
Claude Code use. The manager's final evidence review and Task 1 merge are still
pending; no merge is implied by the observed gate.

At a56e13c, both source-check workflow runs (PR `39061688337`, push
`39061685140`) passed their Python and Rust jobs, per the manager's CI checks.

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
