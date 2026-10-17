# Production acceptance record and runbook

Status: **acceptance and independent review complete; see [PR 8](https://github.com/deanodotdotdot/mirror/pull/8) for merge status**.
The separate Astra review fixed three P2 findings, including ordinary-size prechat
usage access. The final reviewed Python wheels and their checks are attributed
to `7c4884a`; the native builds and live observations remain attributed to
`7371897`, including the original 240-row usage observation. No P2-or-higher
review findings remain. Manager verification and merge are separate.
Reviewed Task 7 is merged at
`072be411ad25b25aa7a82ba9b486db9cecbdd315`; Task 8's first runtime/test/build
candidate was `591e0898df2f37260557c66ed86567cc5c545aab`. Its full source gates
are recorded below; both actual native builds exposed defects in the new
terminal fixture. Corrected candidate
`7371897f9128488a7610f60286a6339241f7273d` now passes actual fresh native-wheel
builds on Mac and Linux. Later changes do not relabel earlier artifacts.
The matrix below records passing deterministic, installed-artifact and live
evidence with explicit limits. Review and manager merge remain separate. Historical
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
| Production login / models / tools / restart / refresh / coexistence | Mac and Linux login/status/catalog/context/usage and initial Claude Code coexistence observations now pass as recorded below. Genuine refresh, both model tool/restart checks, logout and coexistence also pass on both hosts. No live result is inferred from deterministic checks. |

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
Shipping runtime/build sources at `7371897` remain byte-identical to the first
candidate. The later independent-review Python presentation change is recorded
separately below; this equality does not apply to the final whole CLI.

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

## Independent review and final Python artifacts

A separate NEW Astra xhigh reviewer inspected the entire Task 8 delta against
`072be411ad25b25aa7a82ba9b486db9cecbdd315`, the accepted specification, native and
CLI assertions, build hook, foreign-store fixture, opt-in harness, observer code
and the 21 closed production/coexistence evidence records. Three P2 findings
were reproduced or established from the process-launch path and fixed:

- **Early terminal-fixture failure leaked a worker.** Before the first expected
  login notice, no child PIDs had been recorded. Killing the supervisor left a
  native child in a separate session. Cleanup now discovers owned descendants
  while their parent is alive, stops the worker first, permits bounded CLI
  restoration/reaping and closes all PTY/report descriptors. A separate synthetic
  pre-notice failure case asserts both children disappear and its listener closes.
  Actual native Ctrl-C/exit 130/full terminal restoration/listener/file assertions
  remain intact. The test-only correction at `7e3c57a` passed against retained
  `7371897` wheels (Mac 17 plus one Linux-only skip; Linux 18).
- **Prechat usage was inaccessible at ordinary terminal sizes.** The original
  non-scrollable welcome notice clipped earlier report lines at 24 and 65 rows
  in both layouts. Fullscreen reading controls address a hidden transcript there;
  inline scrollback also excludes these live notices. Four real render/key-input
  regressions failed against the old installed CLI. The fix opens the existing
  read-only paged selector header, bounded to the composer height, with Close and
  Escape. It preserves the draft/focus/configuration and creates no conversation,
  model turn, trust or history state. A request owns its loading panel: closing it
  or replacing it prevents late completion from reopening or clearing another
  selector. Other selector defaults and in-conversation output remain unchanged.
- **Refresh-harness timeout missed the detached worker.** Killing only the CLI
  process group cannot reach the installed launcher's `setsid()` worker. The
  harness now discovers owned PID/PPID descendants with bounded output/time,
  terminates descendants first, allows bounded reaping and retains the private
  group guard. A hermetic separate-group worker/listener regression passes.
  Production authority calls, real dedup wait, opt-in and closed outcomes are
  unchanged. Original successful live runs remain attributed to the old harness.

The frozen reviewed source/test revision is
`7c4884ad624c681adab9484dccc6eb753f30aea4`, tree
`4680ded3c7105472fa0ad915ea23a4c81b6a9d31`. Relative to the fresh native-build
revision `7371897`, the only shipping changes are `cli.py`, `selection.py` and
`components/selector.py`. All native/provider/auth/protocol/build inputs remain
identical. Three test files and documentation also changed. This supports reusing
the original native build, unchanged native source gates and production transport,
auth, renewal, model/tool/replay and coexistence observations. It does **not**
claim whole-CLI equality or a fresh native compilation at the reviewed revision.

Both new Python wheels use exact archived `7c4884a` source with the retained
`7371897` worker (including embedded ripgrep), Linux guest and dependency notices.
The build command is `uv build --python 3.12 --wheel --out-dir <wheels>
<source>/mirror-cli`, with `MIRROR_BUILD_RUST_WORKER`, `MIRROR_BUILD_SANDBOXD`,
`MIRROR_THIRD_PARTY_NOTICES` and `MIRROR_WHEEL_PLATFORM` selecting those verified
artifacts. Every installed `mirror/` payload file is compared byte-for-byte to
its new wheel; the native worker, guest and all bundled notices equal the retained
artifacts. No login or provider request was repeated for this presentation change.

| Final reviewed gate | Mac arm64 | Linux aarch64 |
| --- | --- | --- |
| Full Python source CI | 3,402 passed, 23 skipped, Pyright zero errors/warnings; 248.11 seconds | 3,402 passed, 23 skipped, Pyright zero errors/warnings; 195.85 seconds |
| New wheel: exact three `build.sh` installed pytest targets | 17 passed, one Linux-only guest-load skip; 14.65 seconds | 18 passed; 8.46 seconds |
| New wheel: ordinary-size usage and ownership | 10 passed; 8.26 seconds | 10 passed; 8.53 seconds |
| New wheel worker: six native opt-ins | 6 passed | 6 passed |
| Updated Rust acceptance harness, no live opt-in | 4 passed, two intentional ignores | 4 passed, two intentional ignores |

The 23 source skips are six development-native opt-ins and 17 installed-wheel
cases; the separate installed runs above supply those gates. Four known Python
`forkpty` deprecation warnings occurred in each full source run. The harness
ignores the explicitly live test and a synthetic helper exercised by its parent.
The unchanged native workspace result remains the exact `591e089` Linux 839-pass
run; no new full native workspace or passing Mac `/dev/full` claim is made.
Python/Rust formatting and diff checks pass. Optional Ruff 0.16.6 checks retain
exactly the same 16 unrelated pre-existing diagnostics as `7371897`, with no new
diagnostics across the five changed Python files.

| New Python wheel | Mac arm64 | Linux aarch64 |
| --- | --- | --- |
| Filename | `mirror-0.1.0-py3-none-macosx_15_0_arm64.whl` | `mirror-0.1.0-py3-none-linux_aarch64.whl` |
| SHA-256 | `8992bbf5351bf81f34f4fbe81ac502245bb7f7e8961925ad4982cbcb66e83b9c` | `400557cfa7fc828204c072e678dc75e052e2cea443da8351028e4460aab84efd` |
| Installed payload verified | All 187 files | All 187 files |

The embedded worker/guest hashes remain those in the `7371897` artifact table.
Mac review artifacts and closed logs are under
`/tmp/mirror-claude-task8-review/candidate-7c4884a/`, with the full source log at
`/tmp/mirror-claude-task8-review/python-ci-7c4884a.log`; Linux records are under
`/tmp/mirror-claude-manager/pr8-review-7c4884ad624c681adab9484dccc6eb753f30aea4/`.
The [safe evidence index](PRODUCTION-EVIDENCE.json) retains original live hashes
and separately identifies the new artifact manifests and closed review logs.

The four ordinary-size tests use parsed synthetic native metadata with seven
windows, actual rendered frames and Page Up / Page Down key input at 24/65 rows
in both layouts. Six additional cases exercise Escape, replacement and failure
while the request is pending. These establish ordinary-size presentation and
ownership, not new live metadata. The original production default/usage UI
observations required a 240-row terminal and retain that limit. Failed source,
build and controller attempts remain separately recorded rather than relabeled.

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
from the production observations recorded below.

## Live production sequence

After deterministic checks pass, the manager coordinates the user's private
login using the **installed production** commands. Record each platform and
grant separately, and use the same installed artifacts identified above.

| Step | Required observation | Current result |
| --- | --- | --- |
| Mac baseline | Normal Claude Code request using its existing independent grant | Passed 2026-10-17 22:44:40–42 UTC, macOS, `claude-sonnet-4-6`; no external store inspection. Before any production Mirror login. |
| Mac login | Fresh Mirror login without an Anthropic API key; local status and identity/source/profile | Passed on the installed `7371897` Mac wheel: user completed private login; native status is usable, source `Mirror login`, profile `mirror-phase0-omp-df4f996-v1`. |
| Mac tools | Text, read, edit and command turns in a known synthetic workspace; verify file/command effects | Passed: default TUI/exec text with no model/effort override; both models completed real read/edit/bash and verified synthetic file/command effects. The first simple Opus turn lacked thinking; later signed-puzzle/replay evidence is separate. |
| Mac restart | Exit/restart installed Mirror, resume the same bound conversation and complete another request | Opus signed-history replay passed: actual raw-session-ID TUI restart, exact prior record prefix preserved, new read 2 / edit 1 / bash 1, exact effects and unchanged identity. |
| Mac metadata | Native catalog and exact selected-model context, on-demand usage including reported extra-usage setting | Passed: fresh remote catalog with 14 candidates including both required models; exact `model_metadata` context 1,000,000 for each; seven usage windows, extra usage disabled from `spend`, used amount present and limit missing. Identity unchanged. Context is metadata, not a long-request entitlement claim. |
| Two models | `claude-opus-4-8` and available `claude-sonnet-4-6`: signed thinking, tools and replay; successful canonical thinking → text → tools order | Opus passed on both hosts: Mac history contains two signed tool messages/blocks; Linux one. Actual restart preserves every prior record and completes new tools. Sonnet passed on both hosts with one signed tool message/block initially and one after actual restart, plus new read 2 / edit 1 / bash 2. Canonical ordering is inferred from strict production parser acceptance. |
| Linux/SSH | Separate Linux Mirror login with Mac browser and private manual callback, then exec/text/read/edit/command/restart/metadata/usage | Fresh host-local private login passed. Installed metadata passed: fresh remote catalog 14, both exact model contexts 1,000,000, seven usage windows, disabled `spend` extra usage, used amount present/limit missing; identity unchanged. Default TUI and prompt-free exec passed with no model/effort override. Initial simple Opus tools had no signed reasoning. Subsequent puzzle history has one signed tool message/block; a new actual restart preserved its exact prefix and completed read/edit/bash. Sonnet also passed signed-tool/restart checks with exact prefix and effects. |
| Renewal | Production authority refresh, durable revision advance and a fresh installed CLI process completing a normal request | Passed on both hosts: real 61-second dedup wait, production authority refresh, same generation/identity, durable revision 1 → 2, reopened authority and new installed Opus 4.8 no-tool request. Completed 23:05:03.991 UTC Mac / 23:05:04.722 UTC Linux on October 17. |
| Coexistence | Normal Mac Claude Code requests before/after Mirror login, refresh and logout, plus overlapping use | Normal Mac `claude-sonnet-4-6` requests passed at baseline, after Mac Mirror login (22:55:46–48 UTC), and after separate Linux Mirror login (22:57:22–24 UTC on October 17). Normal Mac Claude Code also passed after both refreshes (23:05:14–17 / 23:05:24–26 UTC). Successful normal CLI process intervals overlapped Mac Mirror by 1.330902 seconds and Linux Mirror by 0.797271 seconds. Both Mirror logouts and subsequent normal Mac Claude Code requests passed (23:11:40–41 / 23:11:55–57 UTC). No Linux-local Claude Code or wire-concurrency claim. |

The first minimal-environment Claude Code baseline attempt exited nonzero before
any Mirror login. Public CLI auth status showed that removing normal macOS
user/session variables hid its existing login; restoring only those fields
reported the existing OAuth subscription. The normal CLI marker request then
passed with provider keys, bearer variables and endpoint overrides absent.
This was controller environment setup, not evidence of credential interference.
The successful baseline and after-login requests are separate from the later
refresh/overlap/logout checks. Sanitized evidence is retained in the manager's
`claude-code-baseline-platform-context.json` and
`claude-code-after-mac-login.json`, with the separate-host result in
`claude-code-after-linux-login.json`. Completed production Mac status and metadata
records are `live-mac-7371897/status.jsonl` and `metadata.jsonl`; these contain
only closed outcomes/counts and no credential or provider payload. The matching
Linux record is `live-linux-7371897/metadata.jsonl`.

The first Mac default-TUI controller attempt stopped at its model-picker screen
predicate before issuing its inference task. Its log and database are preserved;
this is not a passing default inference result. The controller incorrectly
required the complete metadata header to be visible simultaneously, although
the selector supports a paged/clipped header. The revised observer checks the
actual visible candidate rows for model ID, fresh/cached state and remote source,
searches each required model without selecting, and emits only fixed booleans
and counts on failure. Synthetic terminal validation passes with the source
header absent. Retries use new UUID-named databases, preserving old evidence. Both Mac and
Linux retries passed the actual model and effort pickers, then stopped at the
usage visibility predicate: the welcome notice is capped at one-third of the
terminal height, so its heading and final limit label were not visible together
at 65 rows. The renderer includes the missing limit as `unavailable`. The
controller now resizes both the real PTY and emulator to 240 rows before usage;
synthetic validation requires that real terminal size. The subsequent live retries passed on both hosts: actual remote model picker,
current-low effort picker and on-demand usage were observed, then the default
TUI and exec completed exact text markers without any model/effort override.
Identity remained unchanged. Closed records are
`live-mac-7371897/default-retry-2.jsonl` and
`live-linux-7371897/default-retry-1.jsonl`.

Both first Opus tool turns then completed, but their controllers could not
recognize completion because the observer required hyphenated UUID spelling.
Native sessions use 32-digit simple UUID hex. The manager stopped those exact
controllers cleanly and preserved their records and failure logs. The corrected
observer validates UUID syntax and exact self-lineage while keeping the original
stored ID for restart. An explicit recovery phase validates the exact first
success marker, API/model/low, signed tool records and synthetic file effects
before resuming the same conversation; it does not repeat the first request.
Synthetic checks cover both UUID spellings and invalid-state rejections before
TUI launch. Both recovery attempts then correctly rejected the original turns:
all tool/file/final checks passed, but there were zero signed thinking blocks
and therefore no signed tool messages. Low adaptive effort does not guarantee
thinking on a simple task. Those records remain tools-only observations.
The next stimulus uses a bounded six-job assignment puzzle, private planning,
independent bash/Python verification, exact solution-file checks and unchanged
strict signature/tool/replay assertions at the same supported low effort. Its
Opus initial turns produced signed blocks, but an additional observer demand for
new signed blocks on every resumed turn was too strict: both resumed turns
completed successfully without adaptive thinking. Those failed runs remain
preserved. A separate third-turn verification now passes on each host after
strict validation of the existing signed history, actual raw-ID TUI restart,
exact prior-prefix comparison and new successful read/edit/bash effects. The
Mac history contains two signed tool messages/blocks and Linux one; both new
verification turns have read 2 / edit 1 / bash 1 and zero new thinking. The check
still rejects unsigned present thinking or invalid provenance. Logs are
`model-opus-replay-verification.jsonl` under each host's live evidence directory.
This proves signed-history replay without requiring the adaptive model to emit
fresh thinking in every later response. No shipping code changed for those
controller corrections or acceptance stimuli. The later independent review found
and fixed the ordinary-size usage presentation defect; the original live
observations remain attributed to `7371897` and its 240-row usage viewport.

The completed Sonnet runs each recorded one signed tool message/block in the
first turn and one in the resumed turn, with new read 2 / edit 1 / bash 2,
verified assignment optima, exact prior-prefix preservation and unchanged
identity. Their closed records are `model-sonnet-reasoning.jsonl` under each
host's live evidence directory. Every observed reasoning block was signed and
had matching model/provider/subscription/profile provenance; no raw signatures
or provider output were retained by this evidence record.

Both actual installed logouts returned `logged_out`. The nine normal Mac
Claude Code observations cover baseline, after each host's login, refresh and
logout, and overlap against each host. The final Linux logout gate uses
`claude-code-after-linux-logout-verified.json` (23:11:55–57 UTC), launched after
confirmed logout completion; an earlier probe is retained but does not establish
that ordering. The Mac logout probe is `claude-code-after-mac-logout.json`
(23:11:40–41 UTC). Both task-owned grants are now locally logged out and all
live processes are closed.

The sanitized [production evidence index](PRODUCTION-EVIDENCE.json) records
artifact identities, safe evidence hashes, both platforms' outcomes, the nine
Claude Code observations and scope limits. These hashes identify closed evidence
files and artifacts, never credentials or signatures. The underlying closed
records are retained under `/tmp/mirror-claude-manager/`; no external credential
store or private terminal transcript was read to produce this ledger.

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
paths. The reviewed timeout/error cleanup discovers only owned descendant
PID/PPID relationships, terminates workers outside the CLI process group before
the CLI, permits bounded reaping and retains the private-group drop guard. A
synthetic separate-group worker/listener regression verifies this path; the live
success records below use the earlier `7371897` harness. Output is
limited to fixed stage/reason fields, closed authority error kinds, revision
numbers, supplied source label, profile, platform, model and time.

Both actual live runs passed using the byte-verified `7371897` artifacts and
matching production harness binaries. Mac completed at
2026-10-17T23:05:03.991356Z (62.80 seconds), Linux at
2026-10-17T23:05:04.721560254Z (63.43 seconds). Each waited the real 61-second
interval, preserved generation/identity, persisted revision 1 → 2 and completed
the fresh installed Opus 4.8 request. Closed logs are retained as
`live-mac-7371897/refresh.log` and `live-linux-7371897/refresh.log` in the
manager's evidence directory. This establishes genuine production refresh
interoperability for these separate host-local grants. The rejected-version trigger exercises production recovery; it does
not pretend a server actually returned 401. Near-expiry timing, races, ambiguous
rotation/crash and actual 401 coordination retain their separate fixture evidence.
The new-process marker is only a normal inference check, not A1 tool execution.

## A1–A11 matrix

The assertions below were inspected in the reviewed native/CLI source. Their
presence alone is not a rerun result; the exact-source source gates above cover
these fixtures. Deterministic runs and live observations remain separate; no live observation is inferred from a fixture pass. Follow [LIVE-RUNBOOK.md](LIVE-RUNBOOK.md) for the installed commands,
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
live Opus and Sonnet signed-history/tool/replay observations now pass as recorded above.
Additional synthetic checks validate optional absent new thinking only after
strict signed-history checks, reject changed prior prefixes and invalid recovery
states, and retain default signature requirements for initial history.

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
claim a live second-account switch. Normal live Claude Code coexistence is separately established by the recorded
baseline, after-login/refresh/logout and overlap requests.

| Gate | Deterministic evidence and its limit | Passing artifact/live evidence and boundary |
| --- | --- | --- |
| A1 fresh login | `auth/tests.rs::login_bootstrap_callbacks_and_generation_fence` and native auth-command tests cover synthetic login/status/commit. Service execution uses synthetic authority/loopback fixtures. | Passed on installed Mac and Linux artifacts: fresh host-local grants, no API keys, default text/exec without effort override, both models' read/edit/command effects and restart/resume. |
| A2 headless | Native `login_wrong_callback_eof_and_signals_leave_no_grant_or_listener` checks wrong-state/cancel/signal cleanup; auth `pending_login_timeout_closes_listener_without_exchange_or_commit` checks timeout without exchange/commit. | Passed actual Linux/SSH browser-on-Mac private manual login, followed by prompt-free exec. Task 7 controlling-PTY/browser-failure and native occupied-port/replayed-state/timeout fixtures are included in the passing source gates; actual native-wheel cancellation now passes on both platforms after the fixture correction. |
| A3 billing | Provider configuration tests reject key fallback; execution tests cover inherited OAuth and explicitly selected API compaction with actual billing labels. Missing-login packaging cases supply conflicting synthetic keys and require failure. | Actual native metadata and TUI usage passed on both hosts: disabled extra usage from spend, used amount present/limit unavailable, with no settings mutation. Source gates include entry-point, child/retry/compaction and credential-conflict fixtures; no forced quota exhaustion or settings mutation is part of live acceptance. |
| A4 renewal | `refresh_race_converges_and_logout_fences_old_lease` asserts revision 2 in both callers, exactly one synthetic refresh and logout fencing. `independent_process_refresh_race_and_death_after_synthetic_rotation` asserts two processes share one replacement, killed rotation preserves old bytes and later authority reports login-required. Atomic fault tests distinguish pre-rename not-committed from post-rename unknown. | Passed on Mac and Linux with matching installed-artifact provenance: real 61-second wait, same generation/identity, durable revision 1 → 2, reopened authority and fresh installed Opus request. No expiry edit or claim of an actual server 401; near-expiry/races/crash remain fixture evidence. |
| A5 coexistence | `foreign_account_switch_cannot_supply_rebind_or_logout_mirror_grant` covers missing-login isolation, unchanged native identity/Mirror generation/revision after two foreign rewrites, and foreign byte preservation through logout, as scoped above. Only disposable fixtures are read. | Mac Claude Code passed before login, after both host logins, after both genuine refreshes and with positive process-interval overlap against each host. Both local logouts and subsequent normal Mac Claude Code requests passed. The file-boundary fixture does not establish Keychain behavior or a live second-account switch. |
| A6 identity | `reconnect_is_explicit_cas_and_duplicates_return_current_state_without_authority` asserts CAS and duplicate/conflicting-ID behavior. `only_a_native_insert_auto_binds_and_resuming_never_rebinds_even_if_empty` covers old/resumed sessions. Storage-failure/restart tests require durable receipt before publication and survive restart. | Both installed models restarted the same native session on both hosts, preserved prior records and identity, and completed new tools. Source gates include authoritative snapshots, attachment Nack retaining history, explicit native reconnect, queued-turn fences, completed-reconnect receipts and exec nonzero/no-prompt failures; real account switching remains deliberately synthetic. |
| A7 wire | OAuth golden/Unicode fixtures pin exact wire bytes and untouched canonical data. `unrepresentable_upstream_interleavings_fail_without_final_or_retry` asserts one terminal error, one attempt, no successful final. Real loopback truncated-tool integration asserts exactly one failed/truncated final before one typed error, one dispatch and zero tool calls. | Passed on both hosts: Opus 4.8 and Sonnet 4.6 signed-history/tool replay with exact prior record preservation and actual new tools. Canonical order is inferred from successful strict-parser acceptance. Source gates include persistence/replay/nonzero checks. Synthetic interleaving rejection is not proof of upstream conformity. |
| A8 models | Catalog fixture asserts two pages, three leases including one rejected revision, no new lease on cache hit. Context precedence validates explicit/exact data and conservative 200,000 for unknown `-1m` names. Detail-without-context cannot hide exact catalog context. | Real picker/catalog/exact context and no-override default Opus startup/exec pass on both hosts. Required Opus 4.8/Sonnet 4.6 signed-tool/replay checks also pass on each host. Source gates include picker/parser/identity/fallback and implicit-startup effort assertions. No Sonnet 5 reasoning claim. |
| A9 usage | Usage fixtures retain nullable windows, percentages over 100, inactive/unknown scopes, disabled extra usage with EUR-scale-3 credits and null limit, independent JPY amount scale, and no inferred currency. Failure test leaves authority usable and models fresh. Cache/backoff tests separately cover reporting reuse. | Actual native metadata and on-demand TUI usage passed on both original `7371897` hosts using a 240-row observer viewport. Independent review fixed ordinary-size prechat access in `7c4884a`: both new installed wheels pass actual-render/Page Up/Down checks at 24/65 rows in both layouts plus asynchronous ownership/draft checks. No new live calls; native metadata/auth/provider behavior is unchanged. Source gates retain local-time/UTC/age/unit/null/extra-usage rendering and identity matching. |
| A10 secrets/endpoints | Native public-clean assertions exclude synthetic secrets from normalized events; new packaging cases check stdout/stderr and all newly created files. Metadata redirect fixture asserts the redirect target receives no connection. Private storage/Debug/transport tests cover mode/link/schema, bounded payloads and fixed endpoints. | Installed seeded-output/new-file canary and real private-TTY cancellation checks pass both fresh wheel builds. Source gates cover private stdin versus argv, bounded/error redaction, all-entry-point custom hosts and provider-switch secrecy. |
| A11 compatibility | Reviewed native protocol adds fields without changing old tags; default HTTP/eval paths fail closed. Existing API/Reflection/Codex tests and OpenAI packaged PTY/tools stay in place. New wheel checks preserve OMP license text. | Both actual native-wheel builds and all packaging checks now pass. Full Linux Rust workspace and both Python source gates passed as recorded above, including binding/launcher checks, main/compactor ACP/remote guards and older saved selections; Mac has the stated Linux-fixture limitation. |

## Documentation completion checklist

The root and CLI READMEs, `mirror-cli/docs/configuration.md`, `architecture.md`,
`building.md` and actual CLI help have been updated against reviewed Task 7.
They explain the separate subscription/API choices, Mirror-owned
grant/private files, host-local SSH login and hidden callback, on-demand usage
and existing extra usage, identity/reconnect, failure recovery, unsupported
surfaces and explicit capability/order limits. Avoid claims beyond the recorded
production observations. [STATUS.md](STATUS.md), this matrix and the sanitized
[production evidence index](PRODUCTION-EVIDENCE.json) record the completed gates.
Acceptance and the separate NEW Astra review are complete. The manager alone
verifies and merges; consult [PR 8](https://github.com/deanodotdotdot/mirror/pull/8)
for its current merge status.
