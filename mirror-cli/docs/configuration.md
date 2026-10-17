# Configuration

Remote gateways are disabled unless supplied with `--remote-endpoint` or
`MIRROR_REMOTE_ENDPOINT`. Use your service operator's endpoint. An empty
environment value disables the remote gateway.

Use `mirror --help` for launch options. Configuration belongs to the terminal
client; each workspace selects its own model and approval settings.

## CLI providers

Select a protocol explicitly when setting up a provider; merely exporting an
API key does not select it.

| Provider                       | `--api`                   | Authentication                                 |
| ------------------------------ | ------------------------- | ---------------------------------------------- |
| Reflection                     | `reflection`              | `/login`, a saved key, or `REFLECTION_API_KEY` |
| OpenRouter                     | `openrouter`              | `OPENROUTER_API_KEY`                           |
| OpenAI Responses               | `openai-responses`        | `OPENAI_API_KEY`                               |
| Anthropic Messages             | `anthropic-messages`      | `ANTHROPIC_API_KEY`                            |
| Claude subscription            | `anthropic-oauth`         | Mirror-owned Claude login                     |
| ChatGPT plan via Codex         | `openai-codex-responses`  | Existing Codex CLI login                       |
| Custom Chat Completions server | `openai-chat-completions` | Endpoint-specific; `--api-key` if required     |

For ChatGPT plan access, log in with the Codex CLI using
`codex login --device-auth`. Mirror reads `~/.codex/auth.json` by default;
`MIRROR_CODEX_AUTH_PATH` can select another credential file. API-key login is
not a substitute for ChatGPT plan authentication.

For a custom endpoint:

```bash
mirror --api openai-chat-completions \
  --base-url http://127.0.0.1:8000/v1 --model your-model-name
```

Without an explicit protocol, saved selection, or model configuration, the CLI
defaults to Reflection, or Chat Completions when a custom endpoint is supplied.

## Claude subscription

Sign in with `mirror auth login claude`, then select `--api anthropic-oauth` or
`/model anthropic-oauth`. Login does not switch your current provider. Bare
`/login` and `/logout` still manage Reflection; use the `claude` argument for
subscription login/logout. Anthropic API remains `anthropic-messages` and uses
its own API key.

Mirror stores one host-local grant in `~/.mirror/claude_oauth.json` (0600), using
`claude_oauth.lock` for process coordination and atomic replacement. Newly
created `~/.mirror` directories are 0700. The Rust authority owns storage and
automatic renewal; do not hand-edit expiry or copy a grant between hosts.
Claude Code/OMP credential stores and bearer environment variables are not
imported. Saved or environment API keys cannot replace this login. Subscription
requests use fixed endpoints; custom base URLs, auth paths and API-key overrides
are invalid for this provider.

On Linux over SSH, keep `mirror auth login claude --no-browser` running, open
its URL on your other computer, then copy the final localhost callback URL from
the browser address bar into the waiting terminal and press Enter. A browser
connection error is expected when the callback listener is on the SSH host.
Input is hidden. Ctrl-C cancels; a fresh login flow needs its own callback.
Never put a callback in chat, a shell argument or a command history file.
Claude Code does not need to be installed on the SSH host.

Use `mirror auth status claude` (or `--json`) to check the local login without
refreshing it. `mirror usage claude` and `/usage claude` fetch usage on demand;
there is no background polling or persistent quota widget. Human output uses
local time and labels freshness; JSON retains UTC and reported units. Missing
windows, limits or currency data remain unavailable. Percentages are not summed
across shared/model windows. Existing extra usage may be enabled on the account;
Mirror displays that setting without changing it or treating it as API fallback.
Usage-reporting failure alone does not disable inference.

New conversations bind to the native account and organization identity. An
existing unbound conversation or changed identity needs `/reconnect claude`
while idle; Mirror preserves the transcript and draft and waits for a durable
native acknowledgment. Login alone does not reconnect. `mirror auth logout
claude` removes only Mirror's local grant; it does not revoke the upstream grant
or change Claude Code. Requests already sent cannot be recalled.

Recovery messages distinguish these cases:

- Missing/revoked login: run `mirror auth login claude` on this host.
- Account/workspace changed: sign in to the intended account, then explicitly
  `/reconnect claude` in the affected conversation.
- Model unavailable: select an available model explicitly; Mirror does not
  silently substitute one. Temporary catalog outages can show labeled stale or
  bundled candidates; missing/revoked auth cannot.
- Subscription limit: wait for the reported retry/reset time or manage the
  account at the provider. Mirror never rotates accounts or falls back to a key.
- Private-store failure: fix ownership/permissions using the reported recovery
  guidance; do not remove locking or copy credentials from another application.

The subscription provider supports local terminal sessions and `mirror exec`.
ACP, remote gateways and evaluation entry points reject it. Exec requires prior
directory trust and login, never prompts during a task, and exits nonzero on
failure. It starts new conversations; use the TUI to resume saved history.

The default is `claude-opus-4-8`. Exact native capability presets supply implicit
`low` effort for Opus 4.8/Sonnet 4.6 and `none` for Sonnet 5. Explicit/saved effort
is retained and may need changing if incompatible with another model. Sonnet 5
reasoning is not verified. Context uses an explicit override, exact model
metadata, or a labeled conservative fallback (200,000); model names never imply
1M entitlement. See the [production evidence](../../docs/claude-integration/ACCEPTANCE.md)
for the tested profile, models and canonical stream-order restriction.

## Model picker

`/model` selects a model; `/effort` selects an effort level. Optional
`~/.mirror/models.json` entries supply model IDs, labels, and routing:

```json
[
  {
    "id": "anthropic/claude-opus-4.8",
    "label": "Opus via OpenRouter",
    "api": "openai-chat-completions"
  },
  {
    "id": "local-model",
    "label": "Local server",
    "api": "openai-chat-completions",
    "base_url": "http://127.0.0.1:8000/v1"
  }
]
```

`id` and `label` are required; `description`, `api`, and `base_url` are optional.
Use `MIRROR_MODELS_CONFIG` to select a different file. Do not put API keys in
model entries or endpoint URLs.

Explicit `--api` and `--base-url` flags override a matching model entry. Without
`--model` or `--api`, startup prefers the workspace's saved model, then the
complete selection in `last_model.json`, including its protocol and endpoint.
That selection need not remain in `models.json`. If no saved selection can be
used, startup falls back to the configured list or the provider's default.
See [model loading](../src/mirror/cli/models.py) and
[startup resolution](../src/mirror/cli/main.py).

## Project instructions and approvals

Put project guidance in `AGENTS.md` or `CLAUDE.md`. Mirror loads instructions
from the repository root through the working directory (only that directory
outside a repository), preferring `AGENTS.md` when both files exist. Global
instructions can live in `~/.mirror/AGENTS.md`. Instructions and available skills
become part of the system prompt.

Manual tool approval is the default unless `auto` was previously saved in the
configuration. Use `mirror --approval-mode approve` or
`mirror --approval-mode auto` to select the mode at launch; explicit mode choices
are remembered across launches. Use `/settings` → **CLI mode** to change the
mode while Mirror is running.
Approval is not filesystem or network isolation.

- Remember: “always allow commands starting with `cat`” → any literal arguments.
  `cat`/`head`/`tail`/`wc` offer command-wide rules; others keep their full prefix.
  Other tools, remote calls and unsupported shell syntax offer exact calls only.
- Match: whole argument words, not text patterns; no shell operators or expansions.
  `cat` does not match `catfish`, `/bin/cat`, or `sudo cat`.
- Scope: local workspace and override, or remote endpoint/transport/session.
- Limits: no file-access restriction or executable identity pinning.
- Storage: `~/.mirror/tool_approvals.json`; exact-call arguments are hashed,
  but command prefixes are stored in plaintext without redaction. Do not remember
  prefixes containing secrets.
- Revoke: stop Mirror → edit file → restart.

## Keybindings

- Approval: Ctrl+C / Escape stop the turn; **Reject** rejects only that call.
- Ctrl+C: dismiss modal → clear draft → interrupt work → exit if idle.
- Escape: dismiss UI or interrupt work.
- Ctrl+D: quit immediately.
- Ctrl+N: choose **Normal** (a new workspace in the current directory) or
  **Worktree** (an isolated checkout), then press Enter. Use Up/Down to choose
  and Escape to cancel. The previous choice is remembered across launches.

Create `~/.mirror/keybindings.json` and restart the CLI. Each entry replaces
one action's shortcuts; omitted actions keep their defaults and `[]` disables
an action's bindings. To move cancel-or-quit to Ctrl+G:

```json
[{ "action": { "type": "CancelOrQuitRequested" }, "keys": [["c-g"]] }]
```

To restore immediate quitting on Ctrl+C while retaining Ctrl+D:

```json
[{ "action": { "type": "QuitRequested" }, "keys": [["c-c"], ["c-d"]] }]
```

Or bind interrupt to Ctrl+C:

```json
[{ "action": { "type": "InterruptRequested" }, "keys": [["c-c"]] }]
```

Escape still dismisses menus. Invalid configuration is reported and ignored.
See [action types](../src/mirror/cli/actions.py) and
[keybinding resolution](../src/mirror/cli/keymap.py).

## Sandbox workspaces

`/sandbox` takes its VM from the `sandbox` object in `~/.mirror/config.json`.
Every field is optional:

```json
{
  "sandbox": {
    "egress": ["github.com", "api.github.com", "pypi.org"],
    "cpus": 2,
    "memory_mib": 2048
  }
}
```

- `image` must be an anonymously accessible OCI reference pinned with
  `@sha256:<64 hex digits>`. Omit it to use the pinned default with Git, curl and
  common build tools. Custom images must provide `python3`, `git`, and `rg`.
  Commands must run as root, which lets `sandboxd` see edits made on this machine
  at once.
- `egress` replaces the default allowlist of GitHub, PyPI and npm hosts. Each
  entry is a host name, or `*.domain` for its subdomains, reached over HTTPS
  only.
- `cpus` is from 1 to 255, and `memory_mib` at least 256.
- The clone's `origin` is the current repository's without credentials: no
  query string, and no user name or password in an HTTPS URL.
- Settings apply to sandboxes created afterwards; a sandbox keeps the VM it was
  created with.

`MIRROR_MSB` names the `msb` to use instead of the first on `PATH`.
`MIRROR_SANDBOXD` names a Linux `sandboxd` to mount instead of the one in the
installed wheel, which a source checkout needs.

## Memory

Mirror can remember your preferences, facts about your repositories, and
procedures that worked. Memory is off until you turn it on. The files are stored
on this machine; relevant memories are included in context sent to the selected
model provider.

`/settings` → **Memory** opens a submenu with the on/off switch and
**Import memories**. Changing the switch applies to future local sessions and
restarts the current session's worker. Finish or interrupt an active turn first.
Press Esc to return to Settings.
The row is absent in remote sessions or when the memory extension is disabled.
`/memory` or `/memory status` shows its status. The command belongs to the
bundled memory extension, and the switch is that extension's setting in
`~/.mirror/config.json`:

```json
{ "extensions": { "builtin/memory": { "config": { "enabled": true } } } }
```

The store is `~/.mirror/memory`. Memories that
should always be in front of the model live as bullets in
`~/.mirror/PINNED_MEMORIES.md`, which the agent's `save_memory` tool writes and
you may edit by hand.

**Import memories** (or `/memory import`) brings in the memories Claude Code
and Codex keep on this machine, under your own name. With `/memory import`,
pass a path, `claude-code=<dir>` or
`codex=<dir>` to choose a source instead of the default locations.
Remote sessions have no memory. See the agent's
[memory guide](../../mirror-agent/docs/memory.md) for what is stored and how
it reaches the model.
