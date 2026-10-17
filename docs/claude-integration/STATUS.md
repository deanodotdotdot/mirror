# Claude subscription implementation ledger

Accepted scope and release gates: [SPEC.md](../../SPEC.md).
Integration branch: `dean/claude-integration` in `deanodotdotdot/mirror`.
Each task has one PR: a `gpt-6.1-sol` xhigh implementer, then an Astra xhigh
review/fix pass. The manager alone merges into the integration branch.

| Task | Depends on | State |
| --- | --- | --- |
| 1. External proof | — | Phase 0 external gate observed/passed at reviewed a56e13c on macOS and Linux/SSH, including manual/random callback, real refresh and Claude Code coexistence. Manager final evidence review/merge pending. See [evidence](../../diagnostics/claude_oauth/EVIDENCE.md). |
| 2. Shared contracts | 1 | Not started; ready after Task 1 final review and manager merge. |
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
manual/random callback fields. The manager alone completes final review and
merges Task 1; it has not been merged. These checks do not establish production
A1–A11. Sonnet 5 reasoning remains unverified; the two-model signed reasoning
gate was met by Opus 4.8 and Sonnet 4.6.
