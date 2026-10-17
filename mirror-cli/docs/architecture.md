# Architecture

Mirror CLI is a Python terminal client. It owns input, rendering, workspace
selection, extensions, and interactive MCP sign-in. It supervises agent workers through
AgentRPC rather than running the model/tool loop in the UI process.

```text
┌──────────────────────────────────────────────┐
│ CLI supervisor                               │
│ workspace tabs · input · rendering            │
│ extensions · connection declarations                  │
└───────────┬─────────────────────┬────────────┘
            │ AgentRPC            │ AgentRPC
┌───────────▼─────────┐ ┌─────────▼───────────┐
│ Worker: main        │ │ Worker: feature     │
│ one conversation    │ │ one conversation    │
└─────────────────────┘ └─────────────────────┘
```

Each started worktree has its own worker and prompt application. Inactive
worktrees keep receiving events while only the selected application renders.
Switching conversations within a worktree closes its worker before starting
the replacement.

## Workspace switching

- Roster/selection: controller. Data/lifecycle: workspace objects.
- Views/effects: CLI root; rendering reads the live roster.
- Exception: extensions retain `context.workspace_controller` for orchestration.
- Creation: immediately selectable; success replaces in place; failures stay.
- Completion preserves identity, order, and selection.
- `/workspaces`: preserve query/highlight; mark removed rows.
- Footer: overflow counts; `Tab` / `Shift+Tab` navigation.
- History: application-owned viewport; scrolling and resizing repaint automatically.
- Shutdown: drain creation/deletion; close agents.

## Source map

| Component | Owns |
| --- | --- |
| [Entrypoint](../src/mirror/cli/main.py) | Arguments, model configuration, and startup |
| [CLI](../src/mirror/cli/cli.py) | Composition/event loop, rendering, input, and AgentRPC sessions |
| [Workspace controller](../src/mirror/cli/workspace_controller.py) | Workspace roster, selection, creation, and persistence |
| [Workspaces](../src/mirror/cli/workspaces.py) | Workspace data and agent-backed lifecycle |
| [Workspace application](../src/mirror/cli/workspace_application.py) | Terminal frame handoff shared by agent and creation views |
| [Local transport](../src/mirror/cli/local_rpc_client.py) | Worker processes and loopback connections |
| [RPC client](../src/mirror/cli/rpc_client.py) | Session requests and incoming events |
| [State](../src/mirror/cli/state.py) | UI state |
| [Actions](../src/mirror/cli/actions.py) | Typed UI actions |
| [Extensions](../src/mirror/cli/extension.py) | Extension state, effects, commands, and lifecycle |
| [Host tools](../src/mirror/cli/host_tools.py) | Tool registration and client-side dispatch |
| [CLI store](../src/mirror/cli/cli_store.py) | CLI history and workspace settings |

## Inline animations

[`Animation`](../src/mirror/cli/components/animation.py) owns frame playback:

```python
loading = animation.Animation(frames=[' ●', '••', '● ', '••'], interval=0.1)
fragments = loading.render()
```

Frames can be plain strings or styled prompt_toolkit text. Playback is indexed
by the monotonic clock, not the number of renders; missed frames are skipped.
`Animation` does not own a timer. prompt_toolkit's shared 0.1-second refresh
interval drives the UI, so visible animations update at 10 frames per second.

## A turn

1. The client starts a worker in the selected workspace and connects to the
   address reported by that worker.
2. A user message enters the bidirectional `Session` stream with a session ID
   and message ID.
3. Runtime deltas update streaming output; committed conversation records are
   handled separately from transient previews.
4. Client-owned extension tools arrive as `host_tool_call` events. Python
   executes them and returns `host_tool_result`. MCP tools instead execute in
   the Rust runtime, without a client callback.
5. Turn completion updates the UI. Other worktrees can continue independently.

Model and reasoning changes are sent as inference controls rather than forcing
a worker restart. Changes during a request apply to the next model request.

## Claude subscription boundary

The Rust worker owns the Claude grant, token renewal, authenticated Messages
transport and catalog/context/usage service. Python does not read credentials
or send subscription HTTP. Subscription and Anthropic API are distinct provider
values; failures cannot select an API key or another account/model implicitly.
The subscription provider is enabled only for local TUI and exec launch paths.

Prechat `mirror auth ... claude` and `mirror usage claude` commands run before
chat logging, workspace trust and model initialization. They start the installed
native worker's `auth --stdio` mode over private JSONL pipes. Login uses a
controlling terminal with hidden callback input and a bounded loopback listener;
Python restores terminal state and reaps the native child on cancellation.
Authorization URLs are transient private login UI, never conversation records.
Other responses contain only validated token-free status/metadata/error fields.

`ClaudeAuthManager` serializes operations with a stable lock and atomically
replaces private grant records. A generation/revision fences login, logout,
refresh and concurrent requests. Metadata uses a separate normalized private
cache with identity/generation/profile keys and request-driven expiration.
Neither cache timers nor usage values trigger inference or billing changes.

Native conversation state owns the account/organization binding. New native
inserts may bind automatically; resumed/unbound histories do not. The UI consumes
authoritative snapshots before accepting provider controls and uses an explicit
compare-and-swap `/reconnect claude` request. A durable receipt and native state
precede acknowledgment, including reconnect retries after a restart. Child
agents, retries and compaction carry the same boundary checks.

The subscription wire adapter preserves opaque reasoning signatures and tool
IDs through durable history/replay. Its representable order is reasoning, text,
then tools. Unsupported interleaving or truncation produces a terminal failure,
not a successful partial answer or repeated tool dispatch. Exact capability
presets constrain reasoning; catalog presence alone is not a capability claim.
See [authority protocol](../../docs/claude-integration/AUTH-PROTOCOL.md),
[transport](../../docs/claude-integration/TRANSPORT.md),
[execution](../../docs/claude-integration/EXECUTION.md) and
[acceptance evidence](../../docs/claude-integration/ACCEPTANCE.md).

## State and recovery

The CLI keeps its history and workspace settings in `~/.mirror/cli.db`. Agent
conversations live separately in `~/.mirror/state.db`; resuming a conversation
loads its stored records. Worker logs live under
`~/.mirror/logs/<conversation-id>/`.

One worker serves a conversation at a time. The worker holds
`~/.mirror/state.db.locks/<conversation-id>.lock` while it runs, so opening a
conversation that another Mirror process has open fails and names that
process. A worker exits when the CLI that started it exits.

`/rewind` forks conversation history; it does not roll back filesystem changes.
`/worktree` creates an independent checkout from `origin/main`; it does not copy
local edits from the current checkout. See the [quickstart](../README.md) for
everyday commands.

## Extension boundary

Extensions are client code, not worker code. They contribute UI state, effects,
commands, host tools, and serializable MCP connection declarations. Local
workers discover and execute those connections; hosted sessions discover their
own authorized gateway catalog. Neither runtime requires a CLI attachment to
execute MCP calls in automatic approval mode.

Extensions and local tools are not sandboxed. Only enable trusted code and
servers. See [Extensions and MCP](extensions.md) for setup and authoring.

### Settings definitions

Settings rows live in `cli/extensions/settings/definitions/`.
`setting_template.py` provides `Setting` for a preference with discrete values,
`Menu` for nested rows, and `Action` for an application action dispatched after
closing the menu. A menu's availability gates its descendants.

Declare a feature's full menu in its definition module and add it to the root
menu in `settings/extension.py`. The settings extension owns generic rendering,
navigation, and applying changes; it does not special-case individual features.
