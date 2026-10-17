# Installed production observations

This is a coordinated, opt-in procedure, **not a record of passing results**.
Run only after the deterministic gates and exact artifact provenance in
[ACCEPTANCE.md](ACCEPTANCE.md) pass. Use the installed wheel's absolute `mirror`
and adjacent Python paths. Each host gets its own fresh Mirror login, private
HOME and disposable synthetic workspace. Do not transfer a grant. The manager
coordinates user browser/terminal login and owns the Linux SSH environment.

The controller must use argv arrays, bounded private output capture and closed
failure messages. Do not print captured stdout/stderr, terminal buffers, provider
responses, signatures, exception strings or callback material on any path.
Keep authorization UI in the user's private terminal/browser. Evidence consists
of source/artifact identifiers, model/profile/platform, counts, booleans and
fixed outcomes. The existing Mac Claude Code CLI is checked through normal
requests only, before/after Mirror login, refresh, logout and during overlap;
never inspect its store and never claim a Linux-local Claude Code observation.

## Initial use and metadata

In the host's synthetic workspace, establish directory trust through installed
Mirror's normal interactive notice. Disable extensions and updates for the test
with `MIRROR_EXTENSIONS_ENABLED=false` and `MIRROR_SKIP_UPDATE=1`. Remove inherited
provider keys, bearer variables, custom endpoints, remote endpoints and
`MIRROR_RUST_WORKER`. Use task-local `MIRROR_CLI_DB` and `MIRROR_MODELS_CONFIG`.

Run the following in the user's private terminal; only sanitized outcomes leave
that terminal. Linux uses `--no-browser` and the callback procedure in ACCEPTANCE.

```bash
"$mirror" auth login claude
"$mirror" auth status claude --json
"$mirror" usage claude
"$mirror" usage claude --json
```

Verify usable status, profile, one identity, and human/JSON usage with reported
extra-usage enabled/disabled/unknown state and units. Do not record the account's
display name/email or change billing settings. Open `/model anthropic-oauth` in
the installed TUI and verify the catalog source/freshness and default selection.
Inspect `/effort` to verify the implicit supported preset; canceling a selection
must preserve the prior model. A saved explicit effort is separate intent.
Before starting a conversation, invoke `/usage claude` at an ordinary terminal
size in both inline and fullscreen layouts. Page Up / Page Down must expose all
reported windows and the extra-usage fields; Close or Escape returns to the
unchanged draft. A large terminal alone cannot establish this access. The original
`7371897` live observer used 240 rows; the later `7c4884a` presentation fix has
separate installed, synthetic actual-render/key-input acceptance at 24 and 65 rows.

A1 requires normal startup and exec **without** `--reasoning-effort` overrides:

```bash
"$mirror" --api anthropic-oauth
"$mirror" exec --api anthropic-oauth 'Reply exactly MIRROR_DEFAULT_EXEC_OK. Do not use tools.'
```

Use actual installed text/read/edit/command turns in the trusted synthetic
workspace as described below; a text marker alone proves none of those tools.
An automated controller captures the marker privately and emits only equality
and return-code outcomes. The two-model reasoning check below is additional.

For exact context and paginated catalog observations, use installed Python's
`mirror.worker.command()` and `mirror.cli.claude_auth.NativeCommand`: `status`
returns a `Completed` whose `.status.identity` supplies `expected_identity`;
`models` and `model_context` return `MetadataCompleted`. Construct parameters
with installed `claude_metadata.params(action, identity, model=...)`, passing
`explicit_context_window=None`. Require the native compatibility profile and
matching identity, then emit only count, presence of the two required model IDs,
state/source/freshness and each exact model's numeric context/provenance. Do not
dump result objects. This uses production metadata/auth interfaces, not a new
HTTP or OAuth client. A catalog row or context number does not prove entitlement
to a long request. Also observe the actual installed picker, rather than claiming
these private commands alone establish UI rendering.

The corresponding closed-output metadata observation is:

```bash
"$installed_python" - <<'PY'
import asyncio, json, sys
from mirror import worker
from mirror.cli import claude_auth as auth, claude_metadata as metadata

PROFILE = 'mirror-phase0-omp-df4f996-v1'
MODELS = ('claude-opus-4-8', 'claude-sonnet-4-6')
async def call(action, params=None):
    async with auth.NativeCommand(worker.command(), action, params=params) as command:
        event = await command.read_event()
        if not isinstance(event, (auth.Completed, auth.MetadataCompleted)):
            raise ValueError()
        if event.compatibility_profile != PROFILE:
            raise ValueError()
        return event

async def main():
    status = await call('status')
    if not isinstance(status, auth.Completed) or status.status.state != 'usable':
        raise ValueError()
    identity = status.status.identity
    catalog = await call('models', metadata.params('models', identity))
    if not isinstance(catalog, auth.MetadataCompleted) or catalog.metadata.identity != identity:
        raise ValueError()
    data = catalog.metadata
    ids = {item.id for item in data.models}
    if data.state not in ('fresh', 'cached') or data.payload['source'] != 'remote' or not set(MODELS) <= ids:
        raise ValueError()
    print(json.dumps(dict(stage='catalog', profile=PROFILE, count=len(ids),
                         required_models_present=True, source='remote', state=data.state)), flush=True)
    for model in MODELS:
        result = await call('model_context', metadata.params('model_context', identity, model=model))
        if not isinstance(result, auth.MetadataCompleted) or result.metadata.identity != identity:
            raise ValueError()
        data = result.metadata
        if data.state not in ('fresh', 'cached') or data.error is not None:
            raise ValueError()
        if data.payload['model'] != model or data.payload['provenance'] != 'model_metadata':
            raise ValueError()
        print(json.dumps(dict(stage='context', model=model, state=data.state,
                             context_window=data.payload['context_window'], provenance='model_metadata')), flush=True)
    usage = await call('usage', metadata.params('usage', identity))
    if not isinstance(usage, auth.MetadataCompleted) or usage.metadata.identity != identity:
        raise ValueError()
    data = usage.metadata
    if data.state not in ('fresh', 'cached') or data.error is not None:
        raise ValueError()
    extra = data.payload['extra_usage']
    print(json.dumps(dict(stage='usage', state=data.state, window_count=len(data.payload['windows']),
                         extra_usage_state=extra['state'], extra_usage_source=extra['source'],
                         amounts_available={key:extra[key] is not None for key in ('used', 'limit')})), flush=True)

try:
    asyncio.run(main())
except BaseException:
    print('{"stage":"metadata_observation_failed"}', flush=True)
    sys.exit(1)
PY
```

This intentionally requires real remote catalog and exact metadata context;
a labeled fallback remains valid product recovery but cannot pass this live
retrieval gate. A single observed catalog page does not prove pagination;
multi-page traversal retains its deterministic fixture evidence.

## Two-model signed reasoning, tool effects and restart

Repeat independently for `claude-opus-4-8` and available `claude-sonnet-4-6` on
each required host. Use a separate fresh database per model, outside any Git
repository, and a marker file `mirror-claude-acceptance.txt` containing exactly
`Mirror production acceptance workspace` plus a newline. Create `fixture.txt`
containing `phase=before` plus a newline. The following are exact TUI entry
commands (`$database` is an absolute task-local SQLite path):

```bash
"$mirror" --database "$database" --api anthropic-oauth \
  --model "$model" --reasoning-effort low --approval-mode auto
```

Only use automatic approval in this disposable workspace. A simple read/edit
prompt may complete without adaptive thinking. For the signed-history gate, use
this bounded assignment problem at the same supported `low` effort. Put each job
A–F in one slot 1–6, minimizing the sum of its row/slot cost:

| Job | 1 | 2 | 3 | 4 | 5 | 6 |
| --- | --- | --- | --- | --- | --- | --- |
| A | 9 | 2 | 7 | 8 | 6 | 4 |
| B | 6 | 4 | 3 | 7 | 5 | 8 |
| C | 5 | 8 | 1 | 8 | 3 | 7 |
| D | 7 | 6 | 9 | 4 | 2 | 5 |
| E | 8 | 5 | 2 | 9 | 7 | 3 |
| F | 4 | 7 | 6 | 3 | 8 | 2 |

Require A before D, B/C nonadjacent, E in an even slot and F not first; break
cost ties by lexicographic order. Ask for private planning before tools, without
printing reasoning. Include the table and constraints in the prompt, then ask:

> Use read to inspect fixture.txt, edit phase=before to phase=after, and bash to
> independently enumerate all 720 orders in Python 3 and verify your candidate
> against the optimum. Write solution-one.txt as order=ORDER;cost=COST followed
> by a newline. Only after verification succeeds, write command-one.txt as
> command-one followed by a newline. Verify the files. Use only these known
> synthetic files, with no network. Reply exactly MIRROR_TOOLS_ONE_OK.

The unique optimum is `BACFDE`, cost 17, among 100 feasible orders. Independently
check exact bytes `phase=after\n`, `command-one\n` and
`order=BACFDE;cost=17\n`; then exit normally. The first turn must include signed
thinking associated with a tool request, valid provenance and successful tool
results. A puzzle solution alone is not signed-reasoning evidence.

Start the observer below with the wheel's adjacent Python. It retains the prior
record bytes only in memory and emits the stored session UUID. Native sessions
use simple 32-digit hex; validate UUID syntax while preserving that exact
spelling for restart and self-lineage checks.
Restart the **installed TUI** with that ID:

```bash
"$mirror" --database "$database" --conversation-id "$session_id" \
  --api anthropic-oauth --model "$model" --reasoning-effort low \
  --approval-mode auto
```

`mirror exec` intentionally does not resume conversations. Do not add resume
semantics or replace the TUI with a custom provider client for this gate.
The manager's TTY controller must bound private capture (for example 1 MiB),
use a finite turn timeout (for example 300 seconds), discard buffers on every
failure, and reap the CLI and child workers. It sends only these synthetic task
strings; it must not replay login callbacks or inspect terminal data for tokens.

Send a second task after restart, repeating the cost table/base constraints and
adding **F before C**. Ask for private planning, followed by:

> Use read on fixture.txt and solution-one.txt, edit phase=after to phase=resumed,
> and bash to independently enumerate the orders and verify the revised optimum.
> Write solution-two.txt as order=ORDER;cost=COST followed by a newline. Only
> after verification succeeds, write command-two.txt as command-two followed by
> a newline. Preserve the first solution and command files. Verify the files;
> use no unrelated files or network. Reply exactly MIRROR_TOOLS_TWO_OK.

The unique revised optimum is `BAFDCE`, cost 24, among 38 feasible orders. Check
exact bytes `phase=resumed\n`, both command files and
`order=BAFDCE;cost=24\n`, with the first solution unchanged. Exit normally, then
create the empty `$resume_done` marker to release the observer's second read.
Successful new tool results/final response and exact preservation of the signed
prior history are required. A restored screen or marker alone is insufficient.
Adaptive thinking may be absent in the new turn; signatures/provenance are still
validated if it appears. Do not expand the frozen effort presets for this test.

If a completed two-turn puzzle was stopped only by an observer overconstraint,
preserve that failed run. A separate verification may validate its signed
history, two user turns, exact second marker and all effects, retain that entire
prefix in memory, and restart the raw stored session ID for a third task:
read the fixture/second solution, edit `phase=resumed` to `phase=verified`, use
bash to write `replay-check.txt` as `replay-verified\n`, preserve the earlier
files and return `MIRROR_TOOLS_THREE_OK`. Check exact prefix preservation plus new
successful read/edit/bash results. This new observation proves replay directly;
it does not retroactively turn the earlier failed controller into a pass.

### Read-only, closed-output observer

Run this block after the first TUI exits, leaving it alive while the controller
restarts the TUI. `$resume_done` must be a new, nonexistent path controlled by
the manager. The observer accesses only the explicitly named synthetic state
database, never auth/metadata stores. Use the installed wheel's Python so its
protobuf schema matches the installed native worker.

```bash
"$installed_python" - "$database" "$model" "$resume_done" <<'PY'
import json, pathlib, sqlite3, sys, time, uuid
from google.protobuf.json_format import MessageToDict
from mirror.protocol import agent_pb2

PROFILE = 'mirror-phase0-omp-df4f996-v1'
def require(condition):
    if not condition:
        raise ValueError()

def snapshot(path):
    require(path.is_absolute() and path.is_file())
    with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True) as db:
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')
        require(db.execute('PRAGMA user_version').fetchone()[0] == 4)
        sessions = db.execute('SELECT id, lineage FROM react_session').fetchall()
        require(len(sessions) == 1)
        sid, lineage = sessions[0]
        require(isinstance(sid, str) and len(sid) in (32, 36))
        uuid.UUID(sid)  # Native sessions use simple hex; retain the exact stored spelling.
        require(json.loads(lineage) == [sid])
        size, count = db.execute(
            'SELECT COALESCE(SUM(LENGTH(payload)),0), COUNT(*) FROM react_entry WHERE session_id=?',
            (sid,),
        ).fetchone()
        require(0 < count <= 500 and size <= 8 * 1024 * 1024)
        rows = db.execute(
            'SELECT sequence, payload FROM react_entry WHERE session_id=? ORDER BY sequence',
            (sid,),
        ).fetchall()
    return sid, rows

def inspect(rows, model, *, require_signed=True):
    calls, results = {}, set()
    counts = dict(users=0, finals=0, signed_blocks=0, signed_tool_messages=0,
                  read=0, edit=0, bash=0)
    last_chat = None
    for _, payload in rows:
        record = agent_pb2.AgentMessage.FromString(payload)
        if record.WhichOneof('payload') != 'chat':
            continue
        chat = record.chat
        kind = chat.WhichOneof('message')
        last_chat = chat
        if kind == 'user':
            counts['users'] += 1
        elif kind == 'assistant':
            require(chat.success and not chat.truncated and record.HasField('inference'))
            require(record.inference.api == 'anthropic-oauth')
            require(record.inference.model == model and record.inference.reasoning_effort == 'low')
            assistant = chat.assistant
            signed = 0
            for detail in assistant.reasoning_details:
                block = MessageToDict(detail)
                require(block.get('type') in ('thinking', 'redacted_thinking'))
                if block['type'] == 'thinking':
                    require(isinstance(block.get('signature'), str) and bool(block['signature']))
                    signed += 1
            if signed:
                provenance = MessageToDict(assistant.provider_fields).get('mirror_reasoning_provenance', {})
                require(provenance.get('schema_version') == 1 and provenance.get('provider_family') == 'anthropic')
                require(provenance.get('model_id') == model)
                require(provenance.get('billing_mode') == 'subscription')
                require(provenance.get('compatibility_profile') == PROFILE)
            counts['signed_blocks'] += signed
            counts['signed_tool_messages'] += bool(signed and assistant.tool_calls)
            for call in assistant.tool_calls:
                require(call.id and call.id not in calls)
                calls[call.id] = call.function.name
            if not assistant.tool_calls and assistant.content.strip():
                counts['finals'] += 1
        elif kind == 'tool':
            tool = chat.tool
            require(chat.success and not chat.truncated)
            require(tool.tool_call_id in calls and tool.tool_call_id not in results)
            results.add(tool.tool_call_id)
            name = calls[tool.tool_call_id]
            if name in ('read', 'edit', 'bash'):
                counts[name] += 1
    require(set(calls) == results)
    required = ('users', 'finals', 'read', 'edit', 'bash')
    if require_signed:
        required += ('signed_blocks', 'signed_tool_messages')
    require(all(counts[key] > 0 for key in required))
    require(last_chat is not None and last_chat.WhichOneof('message') == 'assistant')
    require(bool(last_chat.assistant.content.strip()) and not last_chat.assistant.tool_calls)
    return counts

try:
    database, model, done = pathlib.Path(sys.argv[1]), sys.argv[2], pathlib.Path(sys.argv[3])
    require(model in ('claude-opus-4-8', 'claude-sonnet-4-6'))
    require(done.is_absolute() and not done.exists())
    sid, before = snapshot(database)
    initial = inspect(before, model)
    print(json.dumps(dict(stage='ready_for_restart', session_id=sid, model=model, counts=initial)), flush=True)
    deadline = time.monotonic() + 900
    while not done.exists() and time.monotonic() < deadline:
        time.sleep(0.2)
    require(done.is_file() and done.stat().st_size == 0)
    resumed_sid, after = snapshot(database)
    require(resumed_sid == sid and len(after) > len(before) and after[:len(before)] == before)
    new = inspect(after[len(before):], model, require_signed=False)
    print(json.dumps(dict(stage='restart_replay_passed', model=model, profile=PROFILE,
                         prior_records_unchanged=True, new_counts=new)), flush=True)
except BaseException:
    print('{"stage":"replay_observation_failed"}', flush=True)
    sys.exit(1)
PY
```

Run this observer against synthetic protobuf/SQLite fixtures first to check
schema compatibility and closed failures; this does not establish live success.
The exact prefix comparison checks preservation without writing a second copy
or hashing signatures. It does not expose their contents or sizes. The counters
require signed reasoning associated with a tool request in the initial history,
then actual successful read/edit/bash results in both the initial and resumed
turn. Adaptive low effort may omit new thinking on a later turn. The observer
still validates every present new thinking block and its provenance; it permits
zero new blocks only after strict signed-history validation, actual restart and
exact preservation of the prior records.

Successful inference through the installed production strict stream parser is
evidence that these observed responses obeyed its reasoning → text → tools
ordering restriction. **This is an inference from parser acceptance.** Saved
canonical records do not reconstruct raw SSE chronology, and passing these
requests does not prove every possible upstream response follows that order.
Synthetic interleaving/truncation tests remain separate evidence. Sonnet 5
reasoning remains unproven. The refresh harness's new-process no-tool marker
also remains separate from these signed-thinking/tool/restart checks.
