# Claude subscription implementation ledger

Accepted scope and release gates: [SPEC.md](../../SPEC.md).
Integration branch: `dean/claude-integration` in `deanodotdotdot/mirror`.
Each task has one PR. Tasks 1–3 used `gpt-6.1-sol` xhigh implementation. The user
switched all further implementation to Astra xhigh during Task 4; every task
still receives a separate new Astra xhigh review/fix pass, followed by manager
verification and merge into the integration branch.

| Task | Depends on | State |
| --- | --- | --- |
| 1. External proof | — | Merged as [PR 1](https://github.com/deanodotdotdot/mirror/pull/1) at 2def7dd. Phase 0 gate passed on macOS and Linux/SSH, including manual/random callback, real refresh and Claude Code coexistence. See [evidence](../../diagnostics/claude_oauth/EVIDENCE.md). |
| 2. Shared contracts | 1 | Merged as [PR 2](https://github.com/deanodotdotdot/mirror/pull/2) at 09c1dc9 after Astra review and final Linux verification. Nonsecret contracts, single-attempt authorization plus local snapshot interface, frozen profile, additive protocol support and explicit unsupported/configuration guards. Local validation is recorded below. |
| 3. Credential management | 2 | Merged as [PR 3](https://github.com/deanodotdotdot/mirror/pull/3) at 52f8fdb after independent Astra review and manager verification. Three P2 findings fixed; final Linux verification passed 622 tests plus binding and launcher checks. See [native protocol](AUTH-PROTOCOL.md). |
| 4. OAuth Messages transport | 2 | Merged as [PR 4](https://github.com/deanodotdotdot/mirror/pull/4) at 962fed4 after independent Astra review/fixes and manager verification. Frozen wire profile, strict streaming, normalized replay and bounded typed recovery. See [transport notes](TRANSPORT.md). |
| 5. Model discovery + usage | 3 | Astra implementation complete; independent Astra review/fixes and manager verification underway. |
| 6. Agent execution integration | 3 + 4 + 5 | Astra implementation underway. Independent native storage/protocol/reconnect/stream slice implemented; final metadata wiring waits for reviewed Task 5 merge. See [execution notes](EXECUTION.md). |
| 7. CLI / interactive UX | 2 for initial work; 3 + 5 + 6 to finish | Initial private-command/terminal slice checkpointed; remaining Astra integration waits for Tasks 5–6. |
| 8. Acceptance + packaging + docs | All preceding tasks | Not started. |

Task 7 headless UX must explain that the SSH command stays running, a browser
localhost error on another machine is expected, and the final callback is pasted
into hidden terminal input followed by Enter. Paste shows no characters;
Ctrl-C cancels. This clarification follows the first live SSH attempt.

Fixtures prove local behavior only. The independent macOS and Linux/SSH live
sequences now satisfy the Phase 0 external gate, including directly recorded
manual/random callback fields. Tasks 1–4 are merged; four production tasks (5–8)
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
The manager runs final Linux verification against the reviewed commit.

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
--check` passed. No remaining P2-or-higher review findings. Manager final Linux
verification passed (596 tests, binding and launcher), and Task 2 merged as PR 2.

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
source was changed for these platform limits. The manager's reviewed-head Linux
run passed, including the `/dev/full` fixture. No Task 3 live login, provider request or production
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
touched Rust formatting and diff checks passed. The manager verified reviewed head `c5e4159` on Linux: 622 tests passed, two
intentional tests were ignored, and binding/launcher checks passed. PR 3 merged
as `52f8fdb`; the merged tree exactly matches that reviewed head.

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
No remaining P2-or-higher findings. Final reviewed/integrated Linux verification
remains with the manager, and Task 8 must exercise the approved ordering boundary
with both live acceptance models. No live calls or credential reads were used.


Task 6 independent-slice checkpoint: native SQLite binding and transactional
reconnect receipts, additive reconnect/state/Nack protocol, actor idle/activity
barriers, native provider opt-in, inherited child UUIDs/binding, and persisted
incomplete Claude output are implemented. A real loopback truncated-tool fixture
proves exactly one failed/truncated final record followed by one typed error,
with zero tool execution. Noninteractive regression fixtures return exit 1 for
both failed final records and typed terminal errors. Metadata execution remains
explicitly blocked until the reviewed Task 5 service is merged and connected.

Intermediate Task 6 validation: 482 affected native library tests passed with
one intentional ignore; two protocol wire/projection tests, workspace/all-target
compilation including eval, and nine worker tests passed; Python packaging/typechecking
and the complete CI script passed 3,091 tests with five skips. An earlier broad
library attempt was stopped before the unrelated Unicode-spill test finished;
it is incomplete, not a passing full-suite result. Race, store, protocol, fork and restarted-child UUID checks pass; final
metadata/provider integration checks are pending. No live login,
credential inspection, live provider request or production acceptance was run.
