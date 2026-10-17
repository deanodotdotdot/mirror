# Claude subscription implementation ledger

Accepted scope and release gates: [SPEC.md](../../SPEC.md).
Integration branch: `dean/claude-integration` in `deanodotdotdot/mirror`.
Each task has one PR: a `gpt-6.1-sol` xhigh implementer, then an Astra xhigh
review/fix pass. The manager alone merges into the integration branch.

| Task | Depends on | State |
| --- | --- | --- |
| 1. External proof | — | First live login hit a local macOS terminal defect; synthetic fix ready for review. Live proof pending. See [evidence](../../diagnostics/claude_oauth/EVIDENCE.md). |
| 2. Shared contracts | 1 | Not started; blocked on actual live gate. |
| 3. Credential management | 2 | Not started. |
| 4. OAuth Messages transport | 2 | Not started. |
| 5. Model discovery + usage | 3 | Not started. |
| 6. Agent execution integration | 3 + 4 | Not started. |
| 7. CLI / interactive UX | 2 for initial work; 3 + 5 + 6 to finish | Not started. |
| 8. Acceptance + packaging + docs | All preceding tasks | Not started. |

Fixtures prove local behavior only. Task 1 stays pending until a fresh separate
grant, real refresh, model lookup, text/tool/reasoning replay, local/headless
callbacks and simultaneous Claude Code use are actually observed. No later
implementation or Task 1 merge begins by treating a prepared helper as proof.
