# Claude subscription implementation ledger

Accepted scope and release gates: [SPEC.md](../../SPEC.md).
Integration branch: `dean/claude-integration` in `deanodotdotdot/mirror`.
Each task has one PR. Per the user's 2026-10-17 policy update, all remaining
implementation uses a NEW `gpt-6-astra` xhigh implementer, followed by a separate
NEW Astra xhigh review/fix pass and manager verification. Tasks 1–3 were implemented
before that policy change; Task 4 was completed by an Astra takeover. The manager
alone merges into the integration branch. GitHub Actions remains disabled; all
checks run locally.

| Task | Depends on | State |
| --- | --- | --- |
| 1. External proof | — | Merged as [PR 1](https://github.com/deanodotdotdot/mirror/pull/1) at 2def7dd. Phase 0 gate passed on macOS and Linux/SSH, including manual/random callback, real refresh and Claude Code coexistence. See [evidence](../../diagnostics/claude_oauth/EVIDENCE.md). |
| 2. Shared contracts | 1 | Merged as [PR 2](https://github.com/deanodotdotdot/mirror/pull/2) at 09c1dc9 after independent Astra review and manager Linux verification. One P2 regression was fixed. Nonsecret contracts, single-attempt authorization plus local snapshot interface, frozen profile, additive protocol support and explicit unsupported/configuration guards. Local validation is recorded below. |
| 3. Credential management | 2 | Merged as [PR 3](https://github.com/deanodotdotdot/mirror/pull/3) at 52f8fdbea79e97695a071b5067a5d0cbed088bab after independent Astra review and manager Linux verification; three P2 findings fixed. Rust private-file authority, transactional renewal, PKCE login, local snapshot/status/logout and dedicated native JSONL transport. See [native protocol](AUTH-PROTOCOL.md). |
| 4. OAuth Messages transport | 2 | Merged as [PR 4](https://github.com/deanodotdotdot/mirror/pull/4) at 962fed4361ee79e2af2364267f0627267c543f1a after independent Astra review and manager verification of integrated head 1b9615a1cb375294e01124fd18f73e6ea965b449. Manager Linux checks passed 661 tests with two intentional ignores, plus binding and launcher checks. |
| 5. Model discovery + usage | 3 | Merged as [PR 5](https://github.com/deanodotdotdot/mirror/pull/5) at d4ceee3 after independent Astra review fixed two P2 findings. Manager verified reviewed head 0df4a2e: 695 Linux tests passed, three intentional ignores, binding and launcher passed; merged tree matches exactly. See [protocol](AUTH-PROTOCOL.md) and [service](../../mirror-agent/src/providers/claude/metadata/README.md). |
| 6. Agent execution integration | 3 + 4 + 5 | Independent native slice checkpointed at 81b551f; final metadata routing and integrated checks resume under Astra after manager integration of reviewed Task 5. |
| 7. CLI / interactive UX | 2 for initial work; 3 + 5 + 6 to finish | Astra private-command/TUI checkpoint e66866b is tested. Reviewed Task 5 is integrated for catalog/usage work; final reconnect/runtime wiring waits for reviewed Task 6. |
| 8. Acceptance + packaging + docs | All preceding tasks | Prepared; pending Tasks 5–7. Production A1–A11 remain unproven. |

Task 7 headless UX must explain that the SSH command stays running, a browser
localhost error on another machine is expected, and the final callback is pasted
into hidden terminal input followed by Enter. Paste shows no characters;
Ctrl-C cancels. This clarification follows the first live SSH attempt.

Fixtures prove local behavior only. The independent macOS and Linux/SSH live
sequences now satisfy the Phase 0 external gate, including directly recorded
manual/random callback fields. Tasks 1–5 are merged; three production tasks (6–8)
remain pending review, merge or implementation. These checks do not establish production
A1–A11. Sonnet 5 reasoning remains unverified; the two-model signed reasoning
gate was met by Opus 4.8 and Sonnet 4.6.

Task 2 deliberately contains no credential storage, login, renewal, OAuth HTTP
transport, model discovery, usage request or UI implementation. `anthropic-oauth`
parses as a distinct provider but fails explicitly until those components are
wired; it never resolves an API key or returns metadata fallback success.
The shared contracts live under `mirror-agent/src/providers/claude/`.
`InferenceConfig.claude_session` is an expected per-conversation binding, not
authority to replace one. Task 6 owns native storage keyed by conversation ID
and explicit idle reconnect; shared worktree preferences hold no Claude binding.
Existing conversation/cache IDs remain the request session source.

Validation is local per the user's policy. GitHub Actions is disabled on the
fork; workflow files remain for upstream compatibility. Production A1–A11 are
all unproven until the complete integration is accepted.

Task 2 local macOS checks: workspace/all-target compilation including eval
binding; 78 provider tests; 77 agent-service tests; two nonserializable/nonclone
lease doctests; three binary, 36 headless and nine worker tests; 43 diagnostic
tests; launcher help; and complete Python packaging/typechecking with 3,088
tests passed and five skipped. The headless Linux `/dev/full` fixture was
excluded on macOS; the existing microVM test remains ignored. An earlier full
library run had 540 passes and one new test-helper assertion failure; that
helper was corrected and its complete 77-test agent-service group passed.
The manager completed final Linux verification against the reviewed commit before merging PR 2.

Task 2 independent Astra review fixed a P2 regression in Codex credential-path
resolution. Clearing provider-specific references correctly stopped carrying
`MIRROR_CODEX_AUTH_PATH` into other providers, but also lost the override when
switching back to Codex or explicitly selecting a Codex compactor. Codex now
resolves its own environment override when no explicit path is supplied. An
isolated worker regression reproduced the failed compactor startup before the
fix; a separate isolated subprocess verifies both API-to-Codex and explicit
Codex compactor requests use only the synthetic Codex credential against a
loopback server. Neither test touches
the developer's credentials or mutates the parent process environment.

Reviewer validation: 79 provider tests, 77 agent-service tests, ten worker tests,
two lease doctests, three binary tests, the excluded-HTTP subscription guard,
43 diagnostic tests, and workspace/all-target compilation including eval all
passed. Python packaging/typechecking and the launcher suite passed (64 tests,
one existing installed-worker skip). Touched Rust formatting and `git diff
--check` passed. No remaining P2-or-higher Task 2 review findings. Manager verification of the
merged tree passed 596 Linux tests plus binding and launcher checks.

Task 3 stores only Mirror-owned credentials in `~/.mirror/claude_oauth.json`,
with a separate stable lock. Login/logout change generation; renewal increments
revision, preserves original authorization time and commits before publishing
an access lease. A validated renewal timestamp coordinates the fixed 60-second
recovery grace across processes. A generation/revision-scoped reauthentication
marker records only authoritative refresh revocation; endpoint failures cannot
poison inference auth. Local snapshots order cache inspection without renewing
or revealing tokens. The only nested lock order is metadata -> authority.

The native `mirror-agent auth --stdio` path runs before worker setup and uses
private bounded JSONL pipes, with Python owning browser launch and terminal input.
Login, callbacks and PKCE remain in memory; EOF/signals/cancellation drop pending
work and listeners. Atomic replacement errors distinguish an uncommitted write
from uncertain post-rename durability. No production OAuth Messages provider,
conversation binding, usage/model service or Python UI is connected by Task 3.
These remain assigned to their later tasks; production A1–A11 remain unproven.


Task 3 local macOS validation: the full agent library passed 558 tests with the
synthetic subprocess helper ignored as a standalone test; the final affected
auth suite passed 18 tests and exercises that helper in independent processes.
Four native JSONL/terminal-security tests, ten worker tests, three binary tests,
two lease doctests, 20 protocol tests, 44 sandbox tests, 43 Phase 0 synthetic
tests, launcher help and workspace/all-target compilation including the eval
binding passed. Python packaging/typechecking and `bash ci.sh` passed with 3,088
tests and five skips. Touched Rust formatting and `git diff --check` passed.

The full workspace attempt reached the known Linux-only `/dev/full` headless
fixture on macOS: 36 other headless cases passed. The optional microVM test was
ignored. Additional sandboxd checks need `SANDBOXD_BIN` and a canonical `TMPDIR`;
with those supplied, 45 tests passed and one existing Linux-guest Bash fixture
timed out on macOS (`read -N4` is unsupported by `/bin/bash` there). No unrelated
source was changed for these platform limits. The manager completed reviewed-head Linux
verification before merging Task 3. No Task 3 live login, provider request or production
A1–A11 acceptance was performed.

Task 3 independent Astra review fixed three P2 findings. Login could replace a
grant when cancellation became ready in the same poll as exchange/bootstrap,
and authorization could publish a lease when the final guard made cancellation
ready after a durable refresh. Mutations now check immediately before their
synchronous write, and authorization checks again before returning a lease;
an already committed rotation remains persisted. The private command now reads
ready cancel/EOF/signals before polling an uncommitted operation, while a
selected terminal result still wins subsequent cancellation. Finally, stable
lock and record validation now follow the final asynchronous guard, preventing
a replaced lock from permitting a stale lease after logout under its replacement.

All three issues were reproduced before their fixes. Deterministic regressions
cover exchange/bootstrap cancellation, cancellation after persisted refresh,
lock replacement during a guard wait, preloaded cancel/EOF before process start,
and late cancellation after a committed terminal result. The reviewer's local
macOS authority suite passed 21 tests (the standalone subprocess helper is
ignored and exercised by the process tests), the native private protocol suite
passed five, and the binary suite passed three. No remaining P2-or-higher Task 3
review findings. Workspace/all-target compilation including the Python binding,
touched Rust formatting and diff checks passed. The manager verified reviewed
head `c5e4159` on Linux: 622 tests passed, two intentional tests were ignored,
and binding/launcher checks passed. PR 3 merged as `52f8fdb`; the merged tree
exactly matches that reviewed head.


Task 4 local macOS validation: 115 provider tests (36 new OAuth tests), exact
Bun golden bytes and full-seed hash vectors, 43 diagnostic cases, two lease
doctests, workspace/all-target compilation including the Python/eval binding,
and Python packaging/typechecking CI (3,088 passed, five skipped; zero pyright
errors/warnings). An earlier local agent/protocol test run passed 579 library
cases, three binary, 36 headless and ten worker cases, plus the two doctests.
After the Astra continuation tightened the pre-output/partial-output boundary,
all 115 provider tests and workspace/all-target compilation passed again.
The unchanged long Unicode spill case passed in the earlier broad run
and was excluded from the final repeat; the Linux-only /dev/full headless
fixture was excluded on macOS. The existing opt-in microVM fixture stayed ignored.

Extra broad workspace checks need SANDBOXD_BIN set to a built guest binary.
With that configured and TMPDIR=/private/tmp to avoid Darwin's /var alias,
45 sandboxd cases passed. Its unchanged egress auto-refresh case still uses
read -N4 through /bin/bash, which macOS bash 3.2 rejects; the required Linux
acceptance remains with the manager/Task 8. No unrelated sandbox code was changed.
Task 4 used no real credentials, login or live inference. It enables no global
OAuth runtime path; that remains Task 6. Production A1–A11 remain unproven.
The OAuth decoder rejects thinking after text/tools and text after tools because
Mirror's current canonical message cannot preserve those upstream orderings.
Independent review and Task 8's two-model live gate must check this profile
limitation; rejected streams expose no successful final or executable tools.

Task 4 independent Astra review reproduced and fixed three P2 findings:
overlapping open blocks could reorder streamed text in the canonical completion;
API-key `extra_body.model` overrides used the configured model for reasoning
normalization/provenance; and cancellation after a buffered callback could still
emit tool callbacks and a final completion. Regressions cover text/thinking block
overlap, compatible/incompatible overridden-model history, and cancellation after
text/tool/usage callbacks. All 118 provider tests (39 OAuth module cases) and
workspace/all-target compilation including the Python/eval binding passed.
No remaining P2-or-higher findings. The manager combined the reviewed transport
with Task 3 at `1b9615a`; final Linux verification passed 661 tests with two
intentional ignores, plus binding and launcher checks. PR 4 merged as `962fed4`,
and its tree exactly matches the validated combined head. Task 8 must still
exercise the approved ordering boundary with both live acceptance models.
No live calls or credential reads were used.

Task 5 adds a single `ClaudeMetadataService` backed by `ClaudeAuthorization`.
Catalog and detail HTTP have fixed origins, bounded parsing/pagination and a
shared ten-second budget covering auth, lock waits, renewal and response reads.
Every HTTP attempt gets a fresh lease; one coordinated 401 recovery is allowed.
Context precedence and provenance are explicit, and unknown names never imply
1M. Auth failures cannot become successful offline metadata.

The private normalized metadata cache has a separate stable lock and only the
nested metadata -> authority lock order. Account/workspace, generation and
profile changes invalidate it. Cached reads check the local authority without
renewal; login/logout never wait for metadata. Final model 401/403 errors fence
catalog/context fallback without altering inference credentials. Usage success
is cached for up to 30 seconds; 429 and other reporting failures coalesce with
retry metadata and preserve original stale timestamps. Legacy/generic scopes,
inactive rows, nullable values, extra-usage state and reported units/scales are
preserved without quota gating, account rotation or billing mutations.

Task 5 adds strict `models`, `model_context` and `usage` private JSONL actions,
keeping common compatibility profile, one terminal event, bounded pipes and
EOF/cancel/signal behavior. A narrow filesystem primitive extraction preserves
the credential store's owner/type/mode/symlink protection. Tests use synthetic
values, temporary HOME and test-only loopback; no real credentials, provider
requests or new live login were used. Execution/session wiring belongs to Task 6;
Python display/commands belong to Task 7; production A1–A11 remain deferred.

Task 5 local validation: the broad Rust agent/protocol check passed 646 tests
(587 library, three binary, eight native JSONL, 36 headless, ten worker and two
doctests). Two subprocess-only synthetic helpers and the existing microVM case
were ignored as standalone tests; the helpers are exercised by parent tests.
The known Linux-only `/dev/full` test was excluded on macOS. After the final
metadata changes, all 25 focused metadata tests passed, including real-authority
cross-process coalescing, plus the eight native protocol regressions. Workspace
all-target compilation, including the eval binding, launcher help, 43 Phase 0
synthetic tests, Rust formatting and diff checks passed.

The full Python packaging/typecheck run passed those stages and 3,079 tests,
with five skips and nine unchanged theme/geometry test failures caused by an
inherited `NO_COLOR=1`. The affected four test functions were rerun with
`NO_COLOR` unset and `TERM=xterm-256color`: all ten selected cases passed,
including every failed case. No Python source was changed. These local checks
do not replace the manager's final reviewed/integrated-head Linux verification.

Manager boundary feedback is included in Task 5: stalled endpoint HTTP shares
an absolute nine-second deadline, leaving the last second of the original
ten-second operation budget for local auth/guard checks and labeled fallback.
Auth/lock failures remain hard errors. Tests cover both cached and uncached
stalls, original stale age, and cancellation/guard/revocation/lock failure during
the reserved interval. Retry-After hints are bounded through UTC year 9999;
invalid enormous hints retain the normal cooldown instead of causing timestamp
serialization failure or immediate retries. Production A1–A11 remain unproven.

Manager integrated Task 4 before Task 5 independent review, retaining both
module exports, OMP attributions and validation evidence. Runtime wiring remains
Task 6. The combined Task 5 head awaits independent review and Linux validation.


Task 5 independent Astra review of manager-integrated head `72a609e` against
Task 4 merge `962fed4` reproduced and fixed two P2 findings. A cached/explicit
metadata result could outlive its request guard while waiting for a later
credential snapshot. The additive `local_snapshot_guarded` authority operation
now acquires the credential lock, awaits the guard, synchronously revalidates
the lock/current record, and checks same-poll cancellation before publication.
Its default fails closed; authority wrappers must forward it explicitly.
Ordinary status/local snapshots remain unchanged and no renewal, token exposure
or network request is added. Real-authority regressions cover a revoked guard
during the lock wait, lock replacement during the guard wait, same-poll
cancellation and a near-expiry grant remaining byte-for-byte unchanged.

The second finding let a cached detail without an input context limit hide a
validated limit from the exact catalog model. Valid context now outranks empty
detail metadata on cache hits and after fetches/outages. The selected source's
original timestamp and stale label survive a successful detail response that
still omits the limit and subsequent cache reuse. Both failures were reproduced
before their fixes; public service/result schemas and the ten-second budget
remain unchanged.

Reviewer final focused macOS validation passed 101 Claude tests, with two
subprocess helpers ignored standalone and exercised by parent tests. This covers
the integrated Messages transport, authority, shared contracts and 27 metadata
tests. Eight native JSONL tests, three binary tests, workspace/all-target
compilation including the eval binding, touched Rust formatting and diff checks
also passed. Manager initial Linux verification of pre-fix head `72a609e` passed 689
tests, with three intentional ignores, plus binding and launcher checks. Final
reviewed head `0df4a2e` passed 695 Linux tests, with the same three intentional
ignores, plus binding and launcher checks. PR 5 merged as `d4ceee3`; its tree
exactly matches the reviewed and validated head.
No remaining P2-or-higher findings; no real credentials or live provider calls
were used. Production A1–A11 remain unproven.


Task 7's remaining implementation uses Astra xhigh per the user's updated model
policy; its initial Sol checkpoint is preserved. The private auth client now
honors a native terminal result received during bounded cancellation, including
a completion after the UI requests cancellation. TUI Claude login uses an
isolated hidden buffer and transient display, clears both on Escape/completion,
and restores the prior draft. Explicit login changes the grant only; selecting
the subscription commits a model only after local configuration validation and
usable/refreshable native auth status or successful login. Reflection's bare
login/logout behavior remains separate. At that checkpoint, metadata catalog/usage and authoritative
conversation binding/reconnect still awaited the merged Task 5/6 interfaces.

Task 7 checkpoint validation: the merged Task 1–4 native binary builds in its
own target directory. Final installed-package checks passed 90 focused
Claude/auth/error/model/Reflection/TUI tests, including real native status/logout
with synthetic temporary HOME, actual controlling-PTY manual/loopback completion,
immediate echo restoration, late native completion after Ctrl-C, timeout cleanup,
private link/instruction rendering in both TUI layouts, typed incomplete output,
and late committed login plus Escape preserving the prior model and draft.
A separate launcher/model/updater suite passed 99 tests with one existing
installed-worker skip; its synthetic wheel fixture now includes the lightweight
command-routing module imported by the launcher. Pyright reports zero errors or
warnings, and diff/format checks pass. All checks use synthetic fixtures and no
live provider calls or real credential reads. This is an implementation
checkpoint, not independent review or production acceptance. Full packaged CI
and final native metadata/execution interoperability remain for the integrated
Task 5/6 source before Task 7's single PR is opened.


Task 7's second Astra implementation checkpoint integrates the reviewed and
merged Task 5 metadata protocol. Its dedicated private client validates the
models/model_context/usage result wrapper, schema/provider/action/request ID,
identity, nullable fields and numeric bounds. UTC JSON retains native timestamp
precision; human timestamps use the local timezone. Usage preserves all windows,
inactive rows, percentages above 100, reported extra-usage state and raw amount
units, currencies and scales. Malformed or unknown amount units are unavailable;
there is no guessed currency, conversion, zero balance or unlimited allowance.
The Python usage command renders native degraded states/errors and returns 1
for `unavailable` reports, while retaining the report in `--json` output. Stale
reports remain explicitly labeled with their original fetch time and age.

`/model anthropic-oauth` now obtains the native catalog after local auth checks
and offers labeled candidates without a completion probe or model substitution.
Missing login resumes that requested picker after the private login flow; login
alone does not commit a model. A candidate's observed native identity is checked
again before selection. Escape during discovery or that final local check keeps
the prior provider/model and draft. A cancelled login also resolves a pending
onboarding choice. Both layouts show the offline/bundled candidate labels.
`/usage claude` is on demand and currently labels the current Mirror login;
the adapter accepts explicit expected identity for the authoritative Task 6
conversation snapshot to supply after integration. No binding is inferred here.

This checkpoint's installed-package focused run passed 191 tests, including all
prior private-pipe/controlling-PTY/late-commit cases, login-to-catalog continuation,
Escape after a committed grant, catalog identity changes, provider persistence,
strict metadata parser edge cases, units/nulls/timezones, and usage failure
preserving inference configuration. The rebuilt Task 5 native command passed
real temporary-HOME status/logout and missing-login models/model_context/usage
interoperability, including pre-chat JSON output. Pyright reports zero errors or
warnings; formatting and diff checks pass. No real credentials or live provider
calls were used. Task 6 reconnect/session-state/typed-Nack/runtime wiring and
startup enablement remain gated on its reviewed merge. Full packaged Python CI,
independent review, and the single final Task 7 PR remain outstanding. This is
an implementation checkpoint, not production acceptance.
