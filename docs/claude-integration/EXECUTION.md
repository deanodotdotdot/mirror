# Native Claude execution and conversation identity

Task 6 connects the native local worker to the reviewed Claude authority,
Messages transport, and metadata service. CLI/TUI rendering belongs to Task 7;
production acceptance A1–A11 belongs to Task 8. All Task 6 tests use synthetic
authorities or loopback fixtures. They make no live-provider claim.

## Binding ownership and persistence

SQLite migration 4 adds nullable `react_session.claude_session`. The native
service owns one shared read handle for each conversation; providers only read
it. A binding contains the schema version, account and organization UUIDs, and
compatibility profile. It contains no credential, token generation or secret.

A new SQLite INSERT may bind to the current local Mirror identity when the
initial model or explicit compaction provider selects Claude subscription.
Empty history does not imply a new conversation. An existing unbound session
requires explicit reconnect. A resumed bound session keeps its identity even
when the current Mirror login changes. Login and ordinary inference settings
never rewrite conversation identity. Another provider can remain selected
while reconnect changes the retained Claude binding.

The store commits a compare-and-set before the service publishes the new
binding. A failed commit leaves both unchanged. Successful reconnect receipts
are stored in the same transaction, scoped to the conversation, with version-1
protobuf request encoding. A repeated ID with the same successful request does
not mutate again, including after restart; a conflicting payload is rejected.
Failed requests are remembered for the actor lifetime and are never blindly
acknowledged. Retry a failed operation with a new message ID.

The existing lifetime SQLite conversation lock serializes processes using the
same conversation. Forking copies the binding in the same transaction that
creates the fork, with a fresh conversation ID and no copied reconnect receipts.
Every child/grandchild spawn also receives a fresh native UUID, while inheriting
its parent's binding and authority. Conversation-local task IDs are not request
session IDs.

## Additive native protocol

`ClientMessage.reconnect_claude` uses tag 13. `ReconnectClaude.expected_binding`
(tag 1) is the CAS expectation; absence means unbound. `target_identity` (tag 2)
is required and must equal the current local Mirror authority identity.

`SessionMessage.claude_session_state` uses tag 12. Its `conversation_id`,
`binding`, and optional `message_id` use tags 1, 2, and 3. An absent binding is
explicitly unbound. An absent message ID identifies the initial native snapshot.
A successful reconnect, including a successful duplicate, emits the current
snapshot before `InputAck`. Failures emit `InputNack`, with typed
`claude_error` at tag 4 where applicable; busy uses reason 5. Python's synthetic
`ReplayDone` is not a source of Claude identity.

Reconnect is serialized by the native actor. Its idle barrier includes model
work and compaction, running tasks, child completion input, pending approvals,
host calls, and outstanding external `CallTool` futures. An activity permit is
issued before a tool future is exposed, so even an unpolled future blocks
reconnect. Completion, drop, and failed delivery release the permit. The actor
drains child completions after the task barrier and checks pending inputs again
before committing after local authority validation.

## Execution and terminal outcomes

The native worker opts in explicitly. Default constructors used by HTTP/eval
surfaces reject subscription selection at startup and runtime. Invalid explicit
API keys, credential paths or custom/conflicting endpoints fail before secret
resolution. OAuth never enters the API-key credential resolver. Switching
providers clears provider-specific key/base-URL/auth-path defaults and retains
the conversation binding.

Main requests and retries use the durable identity and current request guard.
Inherited compaction follows the selected provider; an explicitly selected
compaction provider records its actual provider/model. Explicit API-key
compaction remains a separate selection. No account, provider or model is
silently substituted after an OAuth failure.

Context discovery uses `ClaudeMetadataService` with the durable identity and
request guard, including when the user supplies an explicit context window.
The lazy native authority explicitly forwards guarded local snapshots. Explicit
configuration, exact model metadata, reviewed model fallback and conservative
fallback retain the service's precedence and validation; conversion to native
`usize` is checked. Auth, guard, account and storage failures remain terminal.

The agent delegates every subscription context lookup to the metadata service,
which owns cache freshness and identity/generation/profile fencing. Its generic
context-number cache cannot bypass those fences on a later turn. A persisted
`LogRecord`/`Entries` notice precedes context usage and model work and reports
the selected model, limit, provenance, cache state and original source fetch
time. Metadata errors use only closed error/recovery fields. Identical notices
are not repeated; a real state or source change produces a new notice. An
explicit API-key compactor is visibly labeled with its provider, model and
API-key billing before it runs.

The transport's canonical ordering restriction remains: signed thinking must
precede text, which must precede tools. Unsupported interleaving is a typed
terminal compatibility/partial-stream failure, never reordered signed history.
Only a successful transport `Final` carries executable tools. On a terminal
Claude error, the service persists visible partial output as a failed,
truncated transcript record with display-only tool fragments and no executable
calls or manufactured signatures. Fragment-only records remain visible but are
omitted from model replay when they contain no safe assistant content. Partial
compaction text is a display-only log. The actor publishes committed partial
output before the typed `TurnError`; `TurnCompleted` only closes the turn and
does not erase the failure. Native exception-limit exhaustion also remains an
error. Noninteractive consumers must return a nonzero exit for failed final
records or terminal errors.

Exception-limit exhaustion now propagates the actual model error to all native
consumers. The HTTP adapter consequently returns 502 instead of its former 204
after the configured failed-attempt limit; attempt counts and persisted error
traces are retained. Successful API-key/HTTP turns keep their existing success
behavior. This broader error-result correction is covered by the HTTP suite
and is part of Task 6's independent review scope.
