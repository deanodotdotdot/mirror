# Claude subscription implementation ledger

Accepted scope and release gates: [SPEC.md](../../SPEC.md).
Integration branch: `dean/claude-integration` in `deanodotdotdot/mirror`.
Each task has one PR: a `gpt-6.1-sol` xhigh implementer, then an Astra xhigh
review/fix pass. The manager alone merges into the integration branch.

| Task | Depends on | State |
| --- | --- | --- |
| 1. External proof | — | Merged as [PR 1](https://github.com/deanodotdotdot/mirror/pull/1) at 2def7dd. Phase 0 gate passed on macOS and Linux/SSH, including manual/random callback, real refresh and Claude Code coexistence. See [evidence](../../diagnostics/claude_oauth/EVIDENCE.md). |
| 2. Shared contracts | 1 | Implemented and Astra reviewed; one P2 regression fixed, awaiting manager Linux verification/merge. Nonsecret contracts, single-attempt authorization plus local snapshot interface, frozen profile, additive protocol support and explicit unsupported/configuration guards. Local validation is recorded below. |
| 3. Credential management | 2 | Not started. |
| 4. OAuth Messages transport | 2 | Not started. |
| 5. Model discovery + usage | 3 | Not started. |
| 6. Agent execution integration | 3 + 4 | Not started. |
| 7. CLI / interactive UX | 2 for initial work; 3 + 5 + 6 to finish | Not started. |
| 8. Acceptance + packaging + docs | All preceding tasks | Not started. |

Task 7 headless UX must explain that the SSH command stays running, a browser
localhost error on another machine is expected, and the final callback is pasted
into hidden terminal input followed by Enter. Paste shows no characters;
Ctrl-C cancels. This clarification follows the first live SSH attempt.

Fixtures prove local behavior only. The independent macOS and Linux/SSH live
sequences now satisfy the Phase 0 external gate, including directly recorded
manual/random callback fields. Task 1 is merged; seven production tasks (2–8)
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
--check` passed. No remaining P2-or-higher review findings; final full Linux
verification remains the manager's merge gate.
