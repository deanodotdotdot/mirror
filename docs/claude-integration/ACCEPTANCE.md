# Production acceptance record and runbook

Status: **final validation in progress**. Reviewed Task 7 is merged at
`072be411ad25b25aa7a82ba9b486db9cecbdd315`; Task 8's first runtime/test/build
candidate was `591e0898df2f37260557c66ed86567cc5c545aab`. Its full source gates
are recorded below; both actual native builds exposed defects in the new
terminal fixture. Corrected candidate
`7371897f9128488a7610f60286a6339241f7273d` now passes actual fresh native-wheel
builds on Mac and Linux. Later changes do not relabel earlier artifacts.
Production A1–A11 observations remain open. Historical
[Phase 0 evidence](../../diagnostics/claude_oauth/EVIDENCE.md) does not establish
production behavior.

The accepted requirements are [SPEC.md](../../SPEC.md). Compatibility is pinned
to `mirror-phase0-omp-df4f996-v1`, sourced from OMP
`df4f996e66a200ab7e0496114d43bdf85f32194d`. Preserve the MIT attribution in
`mirror-agent/NOTICE` and its packaged copy when building or changing this code.

## Evidence rules

Record the exact clean source commit, platform/architecture, build command and
result, wheel filename and SHA-256, embedded worker SHA-256, installation path,
profile, model IDs, operation outcomes and safe timestamps. Artifact hashes are
appropriate; credential/token hashes are forbidden. A revision supplied to an
acceptance process is a label, not proof of the installed executable's origin.
Link it to the exact build record and artifact hashes before claiming a result.

### Frozen-candidate deterministic results

All results in this table refer to source
`591e0898df2f37260557c66ed86567cc5c545aab`, not later documentation commits.

| Gate | Result |
| --- | --- |
| Linux full Rust workspace | Manager: 839 passed, zero failed, five intentional ignores across 20 suites, with the real built `SANDBOXD_BIN`. The ignores are two subprocess fixture helpers exercised by their parent tests, opt-in live refresh, and two real microVM tests requiring external runtime setup. |
| Linux diagnostic / binding / launcher | Manager: 43 diagnostic fixtures passed, Python binding check and source launcher help passed. |
| Linux Python source CI | Manager: 3,392 passed, 22 skipped, Pyright zero errors/warnings; 200.31 seconds. Skip categories match the Mac source run below. Six development-native opt-ins later passed against the exact `7371897` wheel worker; the native-wheel gate remains separate. |
| Mac Python source CI | 3,392 passed, 22 skipped; Pyright zero errors/warnings; archives, synthetic native packaging hook, generated imports/protobuf and type checks passed. Skips are six development-native opt-ins and 16 installed-wheel cases. Four existing `forkpty` deprecation warnings. |
| Mac development-native opt-ins | All six passed separately against the built candidate worker: private status/logout, metadata missing-login, actual prechat usage, two native attachment/reconnect cases and missing-login exec. |
| Mac Rust agent/protocol | 728 passed across the original run and remaining suites, four intentional ignores. The original run failed only the unchanged `/dev/full` headless fixture, which requires Linux; this is **not** a passing Mac full-workspace claim. Linux's full workspace passed that fixture. The long Unicode spill case completed normally. |
| Mac diagnostic / binding / launcher | 43 diagnostic fixtures passed; Python binding check and source launcher help passed. |
| Mac actual native wheel, first attempt | Actual fresh `build.sh` compiled worker, Linux guest and wheel, then failed installed checks: 14 passed, one Linux-only guest-load skip, two terminal-fixture failures. No accepted artifact retained. |
| Linux actual native wheel, first attempt | Actual fresh `build.sh` compiled worker, guest and wheel, then failed installed checks: 15 passed, two terminal-fixture failures. No accepted artifact retained. |
| Production login / models / tools / restart / refresh / coexistence | Pending manager-coordinated installed-artifact observations. No live result is inferred from the rows above. |

The failed native builds preserve useful findings without counting as acceptance.
Linux passed actual Ctrl-C exit 130, full terminal restoration, process reaping
and both listener closures; its final filesystem assertion incorrectly forbade
the stable private lock created during login preparation. Mac also exposed the
fixture's need to drain the PTY during exit and measure terminal restoration
before its controlling session disappears. The fixture now keeps a supervisor
alive around the actual foreground CLI, reports only exit code/restoration via a
separate pipe, drains bounded private output, checks both processes/listeners,
and permits exactly the 0700 private directory plus 0600 lock after cancellation.
No grant, chat, history, preferences or log file is allowed. Direct real-native
prevalidation passed both ordinary and space-containing helper paths with the
installed Python CLI. This is fixture prevalidation, not native-wheel acceptance.
Shipping runtime/build sources remain byte-identical to the first candidate.

Local raw deterministic logs (synthetic data only) are under
`/tmp/mirror-claude-task8-final-591e089/` on the Mac, and the manager's
`/tmp/mirror-claude-manager/final-linux-591e0898df2f37260557c66ed86567cc5c545aab/`.
The final committed record will contain artifact hashes and sanitized outcomes;
these temporary paths alone are not release evidence that another host can use.

### Passing native artifacts

Both fresh `bash mirror-cli/build.sh <output>` runs used exact archived source
`7371897f9128488a7610f60286a6339241f7273d` (tree
`46a455d4be45a23fa131871e2774d17f33f7ee4f`), Rust 1.95.0, jobs 2, the configured
protobuf compiler and pinned cargo-zigbuild 0.23.4 / ziglang 0.17.0 for the Linux
guest. `build.sh` owns a new Cargo target each time; no development binary is
substituted. The Mac run completed at 2026-10-17 22:42 UTC with **16 passed and
one Linux-only guest-load skip**. The manager's Linux aarch64 run completed at
22:42 UTC with **17 passed**. Existing OpenAI installed PTY/tool tests and all
new native Claude checks ran in both builds, including bundled and helper paths
with spaces. OMP/ripgrep/dependency notices were preserved. Six additional native
opt-ins passed against each retained wheel worker: Mac 6/6 (2.43 seconds),
Linux 6/6. These exercise native private status/logout, metadata missing-login,
prechat usage, both attachment/reconnect variants and safe nonzero exec.

The native worker and Linux guest bytes inside each wheel equal the retained
native artifacts. These SHA-256 values identify artifacts, never credentials:

| Artifact | Mac arm64 | Linux aarch64 |
| --- | --- | --- |
| Wheel filename | `mirror-0.1.0-py3-none-macosx_15_0_arm64.whl` | `mirror-0.1.0-py3-none-linux_aarch64.whl` |
| Wheel SHA-256 | `166d4552081bade0f87a9f781bb03159fe68d07fc48514194e3dc16501dc566a` | `4fe0b34ee40a843a6660dde20c6b9dbed26070d8134ffbbe172777cc12e761c1` |
| Embedded worker SHA-256 | `bdb16c1f1f418b509d4fa875510f4c39a025767e079ae6f755ab304fad70f77c` | `5c7e64f3c0beacdcdc4301ba5d98733ebbbb8d49815acfc6ccfbd423e91a2549` |
| Embedded Linux guest SHA-256 | `eda6b40d0422c945d9b5490faf31ce23ad28d539a9f0f7a8da3f572857497901` | `d720d973292e61222f74d82ff76b9e29c71423cf87fac946e0069ab468344ddf` |

Source checks remain attributed to `591e089`; exact Git comparison proves all
shipping runtime/build files and their executed non-packaged source tests are
unchanged at `7371897`. Only nine Markdown documents and the packaged Claude
fixture differ. Each platform's `reused-gates.json` records that comparison and
original log hashes. The two failed native builds remain recorded separately.

Mac artifacts/manifests/logs are retained under
`/tmp/mirror-claude-task8-final-7371897/`; Linux under
`/tmp/mirror-claude-manager/final-linux-7371897f9128488a7610f60286a6339241f7273d/`.
The Mac wheel is installed at
`/tmp/mirror-claude-task8-final-7371897/live/installed wheel/`. A no-network
preflight compared every installed `mirror/` payload byte against the wheel and
checked the bundled native hash. Its fresh private HOME uses the canonical
`/private/tmp/mirror-claude-task8-final-7371897/live/home` path: native private
storage deliberately rejects symlink ancestors such as macOS `/tmp`.
The same Linux wheel is installed on the isolated SSH host at
`/home/mirror-test/production-7371897/installed wheel/`; the manager verified
all installed package bytes using the same preflight, with a separate fresh
canonical HOME and synthetic workspaces. No production grant or live result is
implied by artifact readiness.

Keep synthetic assertions, actual installed-artifact checks and live observations
separate. A test name or compilation result does not prove an assertion ran.
Store sanitized evidence only: no authorization/callback URL, code, token,
verifier, provider body or credential-store content. Do not inspect, copy or
modify Claude Code/OMP credential files. Normal CLI use is the coexistence probe.

Use disposable workspaces containing only known synthetic files. User login and
callback input take place in private browser/terminal UI, never chat or captured
tool output. Keep Mac and Linux grants independent. Do not force quota exhaustion
or revoke the user's real grant to test an error; use fixtures for account switch,
quota, revocation and crash/error behavior.

## Final deterministic and packaging sequence

1. Integrate reviewed Task 7 into the clean preparatory checkpoint. Inspect its
   final public JSON/human commands, authoritative snapshot/reconnect behavior,
   true controlling-PTY handling and all unsupported launch guards. Resolve any
   real coverage gaps before requesting a live login.
2. Run full source Python CI with `TERM=xterm-256color`, `NO_COLOR` unset and live
   credentials absent. Verify generated protocol bindings and launcher help.
   Retain the workflow's hermetic `diagnostics/claude_oauth/tests` unittest gate;
   passing those diagnostic fixtures does not establish production acceptance.
   Run the final Rust workspace with the real built `mirror-sandboxd` supplied as
   `SANDBOXD_BIN`. Linux is required for the unchanged `/dev/full` and bash `read
   -N` fixtures. Do not label a macOS-incompatible fixture a passing workspace
   check or interrupt the long Unicode-spill case just because a tool poll ends.
3. Run actual `bash mirror-cli/build.sh <output>` on macOS and Linux using the
   reviewed source. It builds fresh worker/ripgrep/Linux guest binaries, generates
   dependency notices, installs the real wheel and runs bundled bash/grep tests,
   both existing OpenAI PTY/tool round trips, sandboxd checks and the new
   `packaged_claude_test.py` suite. Keep all of these checks in the build path.
   Record exact build results and artifact identities on both platforms.
4. The Claude packaging suite checks local status/logout, missing-login usage,
   strict private JSONL, one terminal event, error/output canaries, restrictive
   files, absence of chat/trust/log state and helper paths containing spaces.
   Public status JSON includes the native profile. Both bundled resolution and
   the explicit relocated real-worker path run. The suite rejects a shell stub.
   `ci.sh`'s dummy worker verifies packaging hooks only; it is not native evidence.
5. Run final compatibility/session tests, including older saved sessions and
   additive protocol fixtures, then verify notices/OMP attribution in the actual
   installed wheel. Record all intentional ignores and their reasons. The live
   refresh test stays ignored in every ordinary suite.

These are the core source checks; each host must supply its own prerequisite
paths/toolchain and exact built guest path:

```bash
cargo +1.95.0 build --locked -p mirror-sandboxd
SANDBOXD_BIN=/absolute/build/debug/mirror-sandboxd \
  cargo +1.95.0 test --locked --workspace
env -u NO_COLOR TERM=xterm-256color bash mirror-cli/ci.sh
python3 -m unittest discover -s diagnostics/claude_oauth/tests -v
bash mirror-cli/build.sh /absolute/acceptance-wheel-output
```

The exact source/workspace and native-build outcomes above are recorded separately
from the still-pending production observations.

## Live production sequence

After deterministic checks pass, the manager coordinates the user's private
login using the **installed production** commands. Record each platform and
grant separately, and use the same installed artifacts identified above.

| Step | Required observation | Current result |
| --- | --- | --- |
| Mac baseline | Normal Claude Code request using its existing independent grant | Pending |
| Mac login | Fresh Mirror login without an Anthropic API key; local status and identity/source/profile | Pending |
| Mac tools | Text, read, edit and command turns in a known synthetic workspace; verify file/command effects | Pending |
| Mac restart | Exit/restart installed Mirror, resume the same bound conversation and complete another request | Pending |
| Mac metadata | Native catalog and exact selected-model context, on-demand usage including reported extra-usage setting | Pending |
| Two models | `claude-opus-4-8` and available `claude-sonnet-4-6`: signed thinking, tools and replay; successful canonical thinking → text → tools order | Pending |
| Linux/SSH | Separate Linux Mirror login with Mac browser and private manual callback, then exec/text/read/edit/command/restart/metadata/usage | Pending |
| Renewal | Production authority refresh, durable revision advance and a fresh installed CLI process completing a normal request | Pending |
| Coexistence | Normal Mac Claude Code requests before/after Mirror login, refresh and logout, plus overlapping use | Pending |

Use the existing Mac Claude Code installation for coexistence with the Mac and
Linux Mirror processes. Record those exact platform/process pairings. There is
no Linux-local Claude Code claim and no need to install/authenticate it on SSH
solely for acceptance. Synthetic account-switch independence coverage must be
identified separately from observed same-account coexistence.

For SSH, keep `mirror auth login claude --no-browser` running on Linux. Open its
authorization URL in the Mac browser. Copy the **final localhost URL** with
Cmd-L/Cmd-C even if the browser reports a connection error, paste it into the
waiting SSH terminal with Cmd-V and press Enter; input is hidden. Ctrl-C cancels.
A restarted flow requires its own callback. Never send callbacks through chat.

Success of one text marker is not tool/reasoning/replay evidence. Confirm actual
tool effects and replay for both required models separately. Sonnet 5 reasoning
remains unproven. Catalog metadata does not prove entitlement to long context.
Unsupported upstream block interleaving remains a fail-closed compatibility
boundary, not a general claim that all Anthropic streams use Mirror's order.

## Opt-in production refresh harness

`mirror-agent/tests/claude_production_acceptance.rs` contains the ignored
`live_refresh_persists_and_new_process_request` test. It calls the actual
`ClaudeAuthManager` / `ClaudeAuthorization` interfaces using the existing
Mirror-owned grant. It implements no OAuth exchange, endpoint or credential
serialization of its own and never edits expiry or exposes a refresh flag.

Before running it, verify that the supplied installed `mirror` is the exact
reviewed native wheel. Choose a disposable synthetic workspace that the user
has already trusted through Mirror and in which a normal installed request
succeeds. Create `mirror-claude-acceptance.txt` there containing exactly
`Mirror production acceptance workspace` followed by a newline. The harness
does not grant trust or edit the application config. `HOME` must select the
host's own fresh Mirror login; never transfer a grant between hosts.

Set the following values only for the explicitly requested live run:

```text
MIRROR_CLAUDE_LIVE_ACCEPTANCE=refresh-owned-grant
MIRROR_CLAUDE_ACCEPTANCE_REVISION=<exact source commit, backed by build records>
MIRROR_CLAUDE_ACCEPTANCE_CLI=<absolute installed mirror executable>
MIRROR_CLAUDE_ACCEPTANCE_WORKSPACE=<absolute trusted synthetic workspace>
MIRROR_CLAUDE_ACCEPTANCE_MODEL=claude-opus-4-8
```

Then run the single ignored test from the matching reviewed source:

```bash
cargo +1.95.0 test --locked -p mirror-agent \
  --test claude_production_acceptance \
  live_refresh_persists_and_new_process_request -- --ignored --exact --nocapture
```

The test checks opt-in and paths before authority inspection, reads a local
version, waits a real 61 seconds, rejects concurrent version/identity changes,
and calls `authorize` with that current rejected version. It requires exactly
one persisted revision increment within the same generation/identity and
reopens the production authority to check persistence. The opaque lease is
dropped without inspecting, hashing, logging or dispatching its token.

It then invokes the installed CLI directly by argv (paths with spaces work),
with isolated conversation/model databases and no inherited API credentials or
worker override. The fixed no-tool prompt must return one exact success marker.
Stdout is bounded and never printed; stderr is discarded, including failure
paths. Timeout/error cleanup terminates its private process group. Output is
limited to fixed stage/reason fields, closed authority error kinds, revision
numbers, supplied source label, profile, platform, model and time.

This proves genuine refresh interoperability only after an actual successful
live run. The rejected-version trigger exercises production recovery; it does
not pretend a server actually returned 401. Near-expiry timing, races, ambiguous
rotation/crash and actual 401 coordination retain their separate fixture evidence.
The new-process marker is only a normal inference check, not A1 tool execution.

## A1–A11 matrix

The assertions below were inspected in the reviewed native/CLI source. Their
presence alone is not a rerun result; the exact-source source gates above cover
these fixtures. Deterministic runs and live observations remain separate; no pending live row is replaced by a
fixture pass. Follow [LIVE-RUNBOOK.md](LIVE-RUNBOOK.md) for the installed commands,
read-only protobuf/database observer and two-model TUI restart procedure.

Task 7's inspected assertions include real controlling-terminal restoration,
hidden manual input and browser-open-failure instructions against a synthetic
native worker; private command timeout/cancel result precedence; identity/model
matching and UTC/null/unit-preserving metadata parsing; native state before
attachment Nack; completed reconnect receipt handling; queued-turn fencing;
model persistence only after native Ack; Escape preserving model/effort/draft;
supported implicit startup effort; and nonzero exec after a failed partial
response. Task 8 adds actual installed-native-wheel terminal cancellation and
seeded credential/output/new-file canary checks. Those checks complement the
native port/wrong-state/replayed-state/timeout and execution fixtures rather
than pretending a synthetic worker is the shipped authority.

The observer's four local synthetic checks pass with installed Python protobufs:
successful prefix-preserving resume, changed prior record rejection, unsigned
thinking rejection and missing new tool results rejection. Its signature
canary stays out of stdout/stderr. This validates observation tooling only;
there has been no live signed-thinking/tool/replay observation yet.

The A5 regression `foreign_account_switch_cannot_supply_rebind_or_logout_mirror_grant`
uses fresh native `auth --stdio` processes with a cleared environment and a
disposable HOME. It places only synthetic fixtures at the documented
[Claude Code credential-file path](https://code.claude.com/docs/en/authentication#credential-management)
and OMP's `~/.omp/agent/agent.db`. The Claude Code JSON is a credential/account
sentinel, not a validated application schema. The partial SQLite fixture uses
the pinned OMP
[credential-row representation](https://github.com/can1357/oh-my-pi/blob/df4f996e66a200ab7e0496114d43bdf85f32194d/packages/ai/src/auth/sqlite-credential-store.ts),
without emulating its complete database or migrations.

Foreign presence must leave missing Mirror auth logged out and explicit local
context lookup login-required, with no Mirror store creation. After seeding a
test-owned Mirror grant, two foreign account rewrites must preserve native
status/identity, successful identity-bound local context and every byte of the
Mirror record, including generation/revision. Mirror logout must change only
its own grant and leave every foreign fixture byte unchanged; those foreign
fixtures still cannot authenticate the logged-out Mirror. No provider call or
external app is involved. This is a file-boundary isolation regression; it
does not test macOS Keychain, prove either external app loads the fixtures or
claim a live second-account switch. Normal live Claude Code coexistence remains
a separate required observation.

| Gate | Deterministic evidence and its limit | Remaining artifact/live evidence |
| --- | --- | --- |
| A1 fresh login | `auth/tests.rs::login_bootstrap_callbacks_and_generation_fence` and native auth-command tests cover synthetic login/status/commit. Service execution uses synthetic authority/loopback fixtures. | Installed Mac and Linux fresh grants, text/read/edit/command effects, restart/resume; no API key. |
| A2 headless | Native `login_wrong_callback_eof_and_signals_leave_no_grant_or_listener` checks wrong-state/cancel/signal cleanup; auth `pending_login_timeout_closes_listener_without_exchange_or_commit` checks timeout without exchange/commit. | Actual Linux/SSH browser-on-Mac flow followed by prompt-free exec. Task 7 controlling-PTY/browser-failure and native occupied-port/replayed-state/timeout fixtures are included in the passing source gates; actual native-wheel cancellation now passes on both platforms after the fixture correction. |
| A3 billing | Provider configuration tests reject key fallback; execution tests cover inherited OAuth and explicitly selected API compaction with actual billing labels. Missing-login packaging cases supply conflicting synthetic keys and require failure. | Actual account extra-usage display with no settings mutation. Source gates include entry-point, child/retry/compaction and credential-conflict fixtures; no forced quota exhaustion or settings mutation is part of live acceptance. |
| A4 renewal | `refresh_race_converges_and_logout_fences_old_lease` asserts revision 2 in both callers, exactly one synthetic refresh and logout fencing. `independent_process_refresh_race_and_death_after_synthetic_rotation` asserts two processes share one replacement, killed rotation preserves old bytes and later authority reports login-required. Atomic fault tests distinguish pre-rename not-committed from post-rename unknown. | Successful opt-in production harness, matching installed-artifact provenance and fresh-process request. Real server refresh is unproven until then; fixture timestamps are not live evidence. |
| A5 coexistence | `foreign_account_switch_cannot_supply_rebind_or_logout_mirror_grant` covers missing-login isolation, unchanged native identity/Mirror generation/revision after two foreign rewrites, and foreign byte preservation through logout, as scoped above. Only disposable fixtures are read. | Mac Claude Code requests before/after Mac/Linux Mirror operations and overlap. The file-boundary fixture does not establish Keychain behavior or a live second-account switch. |
| A6 identity | `reconnect_is_explicit_cas_and_duplicates_return_current_state_without_authority` asserts CAS and duplicate/conflicting-ID behavior. `only_a_native_insert_auto_binds_and_resuming_never_rebinds_even_if_empty` covers old/resumed sessions. Storage-failure/restart tests require durable receipt before publication and survive restart. | Installed restart/restore observations. Source gates include authoritative snapshots, attachment Nack retaining history, explicit native reconnect, queued-turn fences, completed-reconnect receipts and exec nonzero/no-prompt failures; real account switching remains deliberately synthetic. |
| A7 wire | OAuth golden/Unicode fixtures pin exact wire bytes and untouched canonical data. `unrepresentable_upstream_interleavings_fail_without_final_or_retry` asserts one terminal error, one attempt, no successful final. Real loopback truncated-tool integration asserts exactly one failed/truncated final before one typed error, one dispatch and zero tool calls. | Both live models' signed-reasoning/tool replay and canonical-order inference. Source gates include persistence/replay/nonzero checks. Synthetic interleaving rejection is not proof of upstream conformity. |
| A8 models | Catalog fixture asserts two pages, three leases including one rejected revision, no new lease on cache hit. Context precedence validates explicit/exact data and conservative 200,000 for unknown `-1m` names. Detail-without-context cannot hide exact catalog context. | Real picker/catalog/exact context, default Opus 4.8 and available Sonnet 4.6 tool/reasoning checks. Source gates include picker/parser/identity/fallback and implicit-startup effort assertions. No Sonnet 5 reasoning claim. |
| A9 usage | Usage fixtures retain nullable windows, percentages over 100, inactive/unknown scopes, disabled extra usage with EUR-scale-3 credits and null limit, independent JPY amount scale, and no inferred currency. Failure test leaves authority usable and models fresh. Cache/backoff tests separately cover reporting reuse. | Actual on-demand account usage, with no timers or billing mutation. Source gates include human/JSON local-time/UTC/age/unit/null/extra-usage rendering and response/request identity matching. |
| A10 secrets/endpoints | Native public-clean assertions exclude synthetic secrets from normalized events; new packaging cases check stdout/stderr and all newly created files. Metadata redirect fixture asserts the redirect target receives no connection. Private storage/Debug/transport tests cover mode/link/schema, bounded payloads and fixed endpoints. | Installed seeded-output/new-file canary and real private-TTY cancellation checks pass both fresh wheel builds. Source gates cover private stdin versus argv, bounded/error redaction, all-entry-point custom hosts and provider-switch secrecy. |
| A11 compatibility | Reviewed native protocol adds fields without changing old tags; default HTTP/eval paths fail closed. Existing API/Reflection/Codex tests and OpenAI packaged PTY/tools stay in place. New wheel checks preserve OMP license text. | Both actual native-wheel builds and all packaging checks now pass. Full Linux Rust workspace and both Python source gates passed as recorded above, including binding/launcher checks, main/compactor ACP/remote guards and older saved selections; Mac has the stated Linux-fixture limitation. |

## Documentation completion checklist

The root and CLI READMEs, `mirror-cli/docs/configuration.md`, `architecture.md`,
`building.md` and actual CLI help have been updated against reviewed Task 7.
They explain the separate subscription/API choices, Mirror-owned
grant/private files, host-local SSH login and hidden callback, on-demand usage
and existing extra usage, identity/reconnect, failure recovery, unsupported
surfaces and explicit capability/order limits. Avoid claims beyond the recorded
production observations. Complete [STATUS.md](STATUS.md) and this matrix with exact
artifact/live results before opening the single Task 8 PR for a new independent Astra review.
