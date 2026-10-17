# Claude OAuth external-contract diagnostic

This is the isolated Phase 0 spike for [SPEC.md](../../SPEC.md#phase-0-prove-the-external-contract).
It has no connection to Mirror's production provider, CLI, models, or credential
format. It uses Python 3.12+ standard-library modules on macOS/Linux. Its fixed
echo tool cannot read files, edit files, or run commands. Full Mirror tool,
compaction, child-agent, retry, identity-binding, and UI acceptance comes later.

The [evidence ledger](EVIDENCE.md) distinguishes fixtures from live observations.
**Passing tests does not pass the live gate.** Do not start later implementation
until the manager resolves that gate. The manager arranges review before real
credentials are used, coordinates the consenting user, and alone merges PRs.

## Synthetic verification

From the repository root:

```sh
python3 -m unittest discover -s diagnostics/claude_oauth/tests -v
```

All tests use synthetic credentials, in-memory transports, private temporary
files, and loopback callbacks. They never contact Anthropic, invoke Claude Code,
or read any existing login; unmocked HTTP and browser launches are blocked by
the test harness. The wire fixture covers Unicode, signed/redacted
reasoning, tool IDs and names, a preexisting tool prefix, forced tool selection,
cache boundaries, and a literal `cch=00000` in user text that must remain intact.
The login regression tests create an actual controlling `/dev/tty` with
`pty.fork()`, use synthetic browser/HTTP responses, and verify hidden manual
input, loopback completion, cancellation and terminal restoration. Callback
waiting uses `SelectSelector`: Darwin's default `KqueueSelector` rejects that
actual terminal even though slave PTYs and pipes work.
Forced tool selection follows OMP's omission of explicit thinking/context
management while retaining low effort. Normal probes let the model select the
tool and retain the adaptive thinking/replay profile.

The fixture's CCH and SHA256 character-index seed were independently verified
with Bun 1.4.3. Normal tests do not need Bun. To deliberately regenerate it:

```sh
python3 diagnostics/claude_oauth/tests/generate_fixture.py /path/to/bun
```

The oracle requires Bun >=1.4, matching the pinned OMP `packageManager` field.
Older Bun 1.2.10 truncates the XXH64 seed to 32 bits and is an invalid oracle
for this profile. The helper implements the full 64-bit seed.

## Coordinated live run

Run login yourself in a private terminal, after the manager has arranged review.
Do not run it inside an agent tool, screen recording, shell `set -x`, or a
terminal recorder: the authorization URL is intentionally shown on `/dev/tty`.
Only the eventual sanitized JSON result goes to stdout. Callback input is hidden
on that terminal, and is never accepted through argv, environment, or chat.
Failures include fixed `stage` and `error_class` labels alongside the safe error
kind and recovery instruction. Give only this sanitized JSON to the manager;
never copy a traceback, provider payload, or browser callback. The labels contain
no exception messages, custom exception names, URLs, codes, tokens or identities.

The default diagnostic directory is
`~/.mirror/diagnostics/claude-oauth-phase0/`. Every new directory is created with
mode `0700`; its grant and stable lock are `0600`, with owner/symlink checks,
atomic replacement, file flush, and directory flush. Directory descriptors are
checked while walking the path; ancestors must be owned by the user or root and
cannot be group/world writable unless protected by a sticky bit (such as `/tmp`).
Symlink path components are rejected. It never opens Claude Code
or OMP storage and never creates an API key. It does not read API-key or bearer
token environment variables. A conflicting `ANTHROPIC_BASE_URL` is rejected;
proxy environment variables are ignored and HTTP redirects are disabled.

Choose a different **fresh diagnostic-only** location with the global
`--state-dir /absolute/private/directory` option if needed. Never point this at
an application credential directory. This format is intentionally distinct from
the future `~/.mirror/claude_oauth.json` production format. Do not transfer this
directory between hosts.

1. Establish a successful tiny Claude Code request using its existing login,
   through Claude Code's own UI/CLI. Record only its version, model, time and
   pass/fail. The helper does not inspect or invoke that application.
2. In the diagnostic repository root, run:

   ```sh
   python3 diagnostics/claude_oauth/proof.py login
   ```

   Complete the browser sign-in with the subscription account. The helper first
   binds loopback `localhost:54545/callback` on IPv4 and IPv6 when available.
   It uses an exact free-port redirect when that port is occupied. Both
   authorization and exchange use the same redirect URI. The final callback
   can also be pasted into the hidden terminal prompt. A browser-launch failure
   leaves this manual path available. The five-minute pending state/verifier
   live only in memory. Ctrl-C cancels, restores terminal echo and closes sockets.
   The old grant remains intact on cancellation or failure before atomic commit.
   If the directory flush fails after replacement, the helper reports
   `credential_commit_outcome_unknown`: inspect status before proceeding; it
   cannot promise durable storage or safely roll back a rotated refresh token.
3. In separate helper processes, run local status, discovery and the first probe:

   ```sh
   python3 diagnostics/claude_oauth/proof.py status
   python3 diagnostics/claude_oauth/proof.py models
   python3 diagnostics/claude_oauth/proof.py probe --model claude-opus-4-8
   ```

   Status is local and does not renew or contact the provider. Each probe makes
   paginated model discovery, a model-detail lookup, then three bounded streamed
   Messages requests: text, one fixed echo tool call, and its result/replay.
   It prints checks and counts rather than model output, signatures, or bodies.
   It reuses one installation identity and one new session ID per probe. A model
   must appear in actual discovery; no automatic model/provider substitution is
   allowed. Missing limits remain unknown. The candidate reasoning map permits
   only `low` adaptive effort, and is pinned rather than inferred from a name.
4. Prove a real refresh and subsequent persistence/replay:

   ```sh
   python3 diagnostics/claude_oauth/proof.py refresh
   python3 diagnostics/claude_oauth/proof.py status
   python3 diagnostics/claude_oauth/proof.py probe --model claude-opus-4-8
   ```

   `refresh` actually calls the token endpoint using only this diagnostic's
   refresh token, then atomically commits before returning success. It does not
   fake expiry. This explicit command is diagnostic; production automatic
   renewal and recovery races are Task 3. A socket timeout has an ambiguous
   outcome if the provider rotated the token before persistence. The old record
   is retained, the helper stops, and a fresh login may be required.
5. Run a probe for a second model that the actual listing exposes. Reviewed
   candidates are `claude-sonnet-4-6` and `claude-sonnet-5`. There is no claim that
   they are available to a particular account. A successful probe with
   `reasoning_gate: pending_no_signed_reasoning_observed` does **not** establish
   reasoning interoperability: a signed block must actually be observed and
   replayed. Stop and review the profile if the provider rejects it; never guess
   a newer client version or silently change the profile to get a pass.
6. Recheck Claude Code after Mirror login and refresh, and run a small Claude
   Code request concurrently with a Mirror probe. Verify both complete. Then
   locally remove only the diagnostic grant and check Claude Code again:

   ```sh
   python3 diagnostics/claude_oauth/proof.py logout
   python3 diagnostics/claude_oauth/proof.py status
   ```

   Logout writes a new-generation tombstone and makes no revoke/billing call.
   It never logs Claude Code/OMP out. Do not force quota exhaustion, revoke a
   grant, change extra-usage settings, or switch another app's account for this
   small Phase 0 run. Full account-switch acceptance remains in Task 8.

Repeat fresh login with `login --random-port` to test upstream acceptance of an
actual fallback port, even when `54545` is free. To prove manual completion use
`login --no-browser`; open the displayed URL elsewhere and paste the provider's
final callback URL or code into this host's hidden prompt. A code-plus-state
format validates both that state and any URL state. Bare provider codes use the
pending state and PKCE verifier; API keys/access tokens are rejected.

Failures include a fixed recovery instruction and optional HTTP status. Keep
the sanitized error for the manager; never obtain a raw provider payload to
diagnose it. A timeout during token exchange/refresh remains an ambiguous
remote outcome even though the local command exits within its budget.

For Linux/SSH acceptance, run the same source on that Linux host and authenticate
there with `--no-browser`; use a browser on a different machine. Do not copy a
laptop grant there. A local `--no-browser` run or a Linux fixture test does not
prove the SSH acceptance gate. Each additional login needs its own observed
outcome. Preserve only sanitized JSON results, and fill in the evidence table
with revision, profile, platform, model IDs, times and outcomes. Never paste
authorization URLs, callback URLs/codes, or `grant.json` into an issue or chat.

## Exact compatibility profile and provenance

Profile: `mirror-phase0-omp-df4f996-v1`; OMP source revision:
`df4f996e66a200ab7e0496114d43bdf85f32194d`.

The protocol constants and shaping in `compatibility.py` adapt these MIT-licensed
OMP files at that revision; [LICENSE-OMP](LICENSE-OMP) preserves their notice:

- `packages/catalog/src/compat/rules/auth/anthropic.kdl`: public client ID,
  authorization URL, six scopes, preferred callback, token and refresh profiles.
  `org:create_api_key` is retained only as an upstream scope; no key-creation API
  or billing mutation exists in this helper.
- `packages/ai/src/registry/engine/oauth-code.ts` and
  `packages/ai/src/registry/oauth/callback-server.ts`: PKCE/state, redirect and
  manual-input conventions. This helper additionally validates all embedded
  states against the pending state and accepts only its exact loopback target.
- `packages/ai/src/registry/oauth/anthropic.ts`: bootstrap identity fallback.
- `packages/ai/src/providers/claude-code-fingerprint.ts`: pinned client `2.1.280`,
  SDK `0.112.1`, transport identity block and underscore tool prefix.
- `packages/ai/src/providers/anthropic.ts`: agent beta list, Stainless profile,
  billing seed, full-seed XXH64 CCH, canonical field order, stable tool/system
  cache boundaries, signed thinking and clear-thinking preservation.
- `packages/ai/src/providers/anthropic-identity.ts`: JSON metadata shape. Device
  identity here uses a Mirror-specific hash domain and this helper's generated
  installation UUID; it never reads another app's installation identity.
- `packages/catalog/src/compat/rules/classes/anthropic.kdl` and
  `packages/catalog/src/compat/resolve.ts` (`defaultSupportsDisplay`): explicit
  candidate reasoning map and model-specific display support. The spike makes
  no context-limit or broader capability claims.

All URLs are fixed `https://claude.ai/oauth/authorize` or reviewed paths on
`https://api.anthropic.com`: `/v1/oauth/token`, `/api/claude_cli/bootstrap`,
`/v1/models[/<id>]`, `/v1/messages`. Authorization/exchange uses no beta/SDK
headers; refresh uses `oauth-2025-04-20` and the SDK OAuth user agent; bootstrap
uses `claude-code/2.1.280`; model lookup uses bearer/OAuth/version headers;
Messages uses the pinned CLI/SDK agent profile, session header and betas.

Deliberate reductions from OMP: no dynamic version adoption, imported credentials,
custom endpoints, overrides, built-in server tools, sampling knobs, model rotation,
fallback credits, cache warming, automatic retries, or production integration.
`fallback-credit-2026-06-01` is only a pinned header; the helper has no redemption
or fallback request path. Responses use `Accept-Encoding: identity` because the
stdlib transport has no SDK decompression; that transport difference requires
live observation. Cache anchors are the stable last tool and last system block;
the spike omits OMP's moving message breakpoints. JSON/model payloads are capped
at 2 MiB, model pages at 20, callbacks at 8 KiB, HTTP socket waits at 10 seconds
(30 for streaming), discovery's total budget at 10 seconds and each streamed
request's at 60. A main-thread POSIX timer also bounds HTTP setup, headers and
body reads against slow byte trickles; callbacks get a one-second connection
budget within the five-minute login lifetime. The CLI is the supported entry
point, not a background-thread library. Production async cancellation remains
a Task 3/4 requirement.

This profile is an interoperability hypothesis, not an official third-party API
contract or Anthropic endorsement. A server rejection or a missing required
endpoint capability blocks the accepted design until the manager and user
resolve it; shared credentials, Claude Code backend, proxies, API keys, or
unreviewed version changes are not substitutes.
