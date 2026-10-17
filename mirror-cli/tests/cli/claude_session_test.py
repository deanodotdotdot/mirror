"""Native session authority, correlated controls, and development-worker recovery."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import pathlib
import sqlite3
from collections.abc import AsyncIterator
from unittest import mock

import grpc
import pytest

from mirror.cli import claude_auth, claude_session, local_rpc_client, rpc_client
from mirror.protocol import agent_pb2, agent_pb2_grpc

_IDENTITY = {
  'account_id': '11111111-1111-4111-8111-111111111111',
  'organization_id': '22222222-2222-4222-8222-222222222222',
}
_OTHER = dict(_IDENTITY, account_id='33333333-3333-4333-8333-333333333333')
_BINDING = claude_session.Binding(
  **_IDENTITY, compatibility_profile='fixture-v1'
)
_OTHER_BINDING = claude_session.Binding(
  **_OTHER, compatibility_profile='fixture-v1'
)


def _state(
  binding: claude_session.Binding | None, message_id: str | None = None
) -> agent_pb2.SessionMessage:
  state = agent_pb2.ClaudeSessionState(conversation_id='conversation')
  if binding is not None:
    state.binding.CopyFrom(binding.to_proto())
  if message_id is not None:
    state.message_id = message_id
  return agent_pb2.SessionMessage(claude_session_state=state)


def _ack(message_id: str) -> agent_pb2.SessionMessage:
  return agent_pb2.SessionMessage(
    input_ack=agent_pb2.InputAck(message_id=message_id)
  )


def _nack(
  message_id: str, kind: str = 'account_changed'
) -> agent_pb2.SessionMessage:
  return agent_pb2.SessionMessage(
    input_nack=agent_pb2.InputNack(
      message_id=message_id,
      error='UNTRUSTED_NATIVE_ERROR_CANARY',
      claude_error=agent_pb2.ClaudeErrorDetails(
        schema_version=1,
        kind=kind,
        retry_disposition='terminal',
        recovery='reconnect',
        compatibility_profile='fixture-v1',
      ),
    )
  )


class _Server(agent_pb2_grpc.AgentRPCServicer):
  def __init__(self) -> None:
    self.failure: str | None = None
    self.requests: asyncio.Queue[agent_pb2.ClientMessage] = asyncio.Queue()
    self.responses: asyncio.Queue[agent_pb2.SessionMessage] = asyncio.Queue()
    self.records = [
      agent_pb2.AgentMessage(
        id='history',
        chat=agent_pb2.ChatRecord(
          user=agent_pb2.UserMessage(content='Retained history'), success=True
        ),
      )
    ]

  async def Session(self, requests, context):
    async def read():
      async for request in requests:
        self.requests.put_nowait(request)

    task = asyncio.create_task(read())
    try:
      while True:
        response = await self.responses.get()
        if self.failure is not None:
          await context.abort(grpc.StatusCode.INTERNAL, self.failure)
        yield response
    finally:
      task.cancel()
      await asyncio.gather(task, return_exceptions=True)

  async def GetMessages(self, request, context):
    return agent_pb2.GetMessagesResponse(
      messages=self.records, total=len(self.records)
    )


class _Transport:
  def __init__(self, port: int):
    self.channel = grpc.aio.insecure_channel(f'127.0.0.1:{port}')

  async def connect(self, host_tools=()):
    return self.channel

  async def close(self):
    await self.channel.close()


@contextlib.asynccontextmanager
async def _running(
  *, bound: bool = True
) -> AsyncIterator[
  tuple[
    rpc_client.Client, _Server, list[agent_pb2.SessionMessage], mock.AsyncMock
  ]
]:
  service = _Server()
  server = grpc.aio.server()
  agent_pb2_grpc.add_AgentRPCServicer_to_server(service, server)
  port = server.add_insecure_port('127.0.0.1:0')
  await server.start()
  received = []
  error = mock.AsyncMock()
  client = rpc_client.Client(
    _Transport(port),
    'conversation',
    on_message=received.append,
    on_error=error,
    inference_config=agent_pb2.InferenceConfig(
      api='anthropic-oauth', model='selected'
    ),
  )
  startup = asyncio.create_task(
    client.start(replay=True, wait_for_attachment=True)
  )
  try:
    attachment = await asyncio.wait_for(service.requests.get(), 3)
    assert not attachment.set_inference_config.HasField('claude_session')
    service.responses.put_nowait(_state(_BINDING if bound else None))
    service.responses.put_nowait(_nack(attachment.message_id, 'login_required'))
    await asyncio.wait_for(startup, 3)
    yield client, service, received, error
  finally:
    await client.close()
    await server.stop(None)
    await asyncio.gather(startup, return_exceptions=True)


@pytest.mark.asyncio
@pytest.mark.parametrize('bound', [True, False])
async def test_attachment_nack_retains_native_snapshot_history_and_explicit_reconnect(
  bound: bool,
) -> None:
  async with _running(bound=bound) as (client, service, received, error):
    assert client.claude_snapshot == claude_session.Snapshot(
      'conversation', _BINDING if bound else None
    )
    assert any(r.agent_message.id == 'history' for r in received)
    assert any(r.HasField('replay_done') for r in received)
    assert any(r.HasField('input_nack') for r in received)
    error.assert_not_called()
    operation = asyncio.create_task(client.reconnect_claude(_OTHER))
    request = await asyncio.wait_for(service.requests.get(), 3)
    assert request.reconnect_claude.HasField('expected_binding') == bound
    assert (
      request.reconnect_claude.target_identity.account_id
      == _OTHER['account_id']
    )
    service.responses.put_nowait(_ack('unrelated'))
    service.responses.put_nowait(_state(_OTHER_BINDING, 'unrelated'))
    await asyncio.sleep(0.02)
    assert not operation.done()
    assert client.claude_snapshot.binding == (_BINDING if bound else None)
    service.responses.put_nowait(_state(_OTHER_BINDING, request.message_id))
    await asyncio.sleep(0.02)
    assert not operation.done()  # State alone is not a success receipt.
    service.responses.put_nowait(_ack(request.message_id))
    assert (await asyncio.wait_for(operation, 3)).binding == _OTHER_BINDING
    service.responses.put_nowait(_state(_BINDING, request.message_id))
    service.responses.put_nowait(_ack(request.message_id))
    await asyncio.sleep(0.02)
    assert client.claude_snapshot.binding == _OTHER_BINDING
  assert client.claude_snapshot is None


@pytest.mark.asyncio
@pytest.mark.parametrize('api', ['anthropic-oauth', 'anthropic-messages'])
@pytest.mark.parametrize('claude_compaction', [False, True])
@pytest.mark.parametrize('failure', ['nack', 'rpc', 'protocol'])
async def test_untyped_claude_attachment_rejection_does_not_echo_native_payload(
  api: str,
  claude_compaction: bool,
  failure: str,
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  service = _Server()
  server = grpc.aio.server()
  agent_pb2_grpc.add_AgentRPCServicer_to_server(service, server)
  port = server.add_insecure_port('127.0.0.1:0')
  await server.start()
  errors = mock.AsyncMock()
  client = rpc_client.Client(
    _Transport(port),
    'conversation',
    on_message=lambda _: None,
    on_error=errors,
    inference_config=agent_pb2.InferenceConfig(api=api, model='selected'),
    claude_compaction=claude_compaction,
  )
  if failure == 'protocol':
    monkeypatch.setattr(
      client,
      '_observe_claude',
      mock.Mock(side_effect=ValueError('SYNTHETIC_PRIVATE_NATIVE_PAYLOAD')),
    )
  elif failure == 'rpc':
    service.failure = 'SYNTHETIC_PRIVATE_NATIVE_PAYLOAD'
  startup = asyncio.create_task(
    client.start(replay=True, wait_for_attachment=True)
  )
  try:
    request = await asyncio.wait_for(service.requests.get(), 3)
    service.responses.put_nowait(
      agent_pb2.SessionMessage(
        input_nack=agent_pb2.InputNack(
          message_id=request.message_id,
          error='SYNTHETIC_PRIVATE_NATIVE_PAYLOAD',
        )
      )
    )
    with pytest.raises(RuntimeError) as raised:
      await asyncio.wait_for(startup, 3)
    visible = str(raised.value) + str(errors.call_args_list)
    assert ('SYNTHETIC_PRIVATE_NATIVE_PAYLOAD' in visible) == (
      api != 'anthropic-oauth' and not claude_compaction
    )
  finally:
    await client.close()
    await server.stop(None)
    await asyncio.gather(startup, return_exceptions=True)


@pytest.mark.asyncio
async def test_claude_sensitive_worker_keeps_late_transport_errors_private_after_switch() -> (
  None
):
  async with _running() as (client, service, received, errors):
    client.send(
      agent_pb2.ClientMessage(
        message_id='ordinary-switch',
        set_inference_config=agent_pb2.InferenceConfig(
          api='anthropic-messages', model='ordinary'
        ),
      )
    )
    request = await asyncio.wait_for(service.requests.get(), 3)
    assert request.set_inference_config.api == 'anthropic-messages'
    service.responses.put_nowait(_ack(request.message_id))
    async with asyncio.timeout(3):
      while not any(
        r.input_ack.message_id == request.message_id for r in received
      ):
        await asyncio.sleep(0)
    assert client.claude_sensitive
    service.failure = 'SYNTHETIC_PRIVATE_LATE_ERROR'
    service.responses.put_nowait(agent_pb2.SessionMessage())
    async with asyncio.timeout(3):
      while not errors.called:
        await asyncio.sleep(0)
    assert 'SYNTHETIC_PRIVATE_LATE_ERROR' not in str(errors.call_args_list)
    assert 'Claude worker connection failed' in str(errors.call_args_list)


@pytest.mark.asyncio
@pytest.mark.parametrize('accepted', [False, True])
async def test_completed_reconnect_ignores_receipts_before_waiter_cleanup(
  accepted: bool,
) -> None:
  async with _running() as (client, service, _, _):
    operation = asyncio.create_task(client.reconnect_claude(_OTHER))
    request = await asyncio.wait_for(service.requests.get(), 3)
    # Buffered responses can be processed without yielding to the waiter that
    # removes the pending request. A completed control is no longer authority.
    if accepted:
      client._observe_claude(_state(_OTHER_BINDING, request.message_id))
    client._observe_claude(
      _ack(request.message_id) if accepted else _nack(request.message_id)
    )
    client._observe_claude(
      _state(_BINDING if accepted else _OTHER_BINDING, request.message_id)
    )
    assert client.claude_snapshot == claude_session.Snapshot(
      'conversation', _OTHER_BINDING if accepted else _BINDING
    )
    if accepted:
      assert (await operation).binding == _OTHER_BINDING
    else:
      with pytest.raises(claude_session.ControlError):
        await operation


@pytest.mark.asyncio
@pytest.mark.parametrize(
  'failure', ['nack', 'busy', 'ack_without_state', 'disconnect', 'timeout']
)
async def test_reconnect_failure_never_fabricates_target(
  failure: str, monkeypatch: pytest.MonkeyPatch
) -> None:
  async with _running() as (client, service, _, _):
    if failure == 'timeout':
      monkeypatch.setattr(rpc_client, 'CLAUDE_CONTROL_TIMEOUT', 0.05)
    operation = asyncio.create_task(client.reconnect_claude(_OTHER))
    request = await asyncio.wait_for(service.requests.get(), 3)
    if failure == 'nack':
      service.responses.put_nowait(_nack(request.message_id))
    elif failure == 'busy':
      service.responses.put_nowait(
        agent_pb2.SessionMessage(
          input_nack=agent_pb2.InputNack(
            message_id=request.message_id,
            reason=agent_pb2.INPUT_NACK_REASON_CONVERSATION_BUSY,
          )
        )
      )
    elif failure == 'ack_without_state':
      service.responses.put_nowait(_ack(request.message_id))
    elif failure == 'disconnect':
      await client.close()
    with pytest.raises(claude_session.ControlError) as raised:
      await asyncio.wait_for(operation, 3)
    assert 'UNTRUSTED' not in str(raised.value)
    assert client.claude_snapshot == (
      claude_session.Snapshot('conversation', _BINDING)
      if failure in ('nack', 'busy')
      else None
    )


@pytest.mark.asyncio
@pytest.mark.parametrize('outcome', ['ack', 'nack', 'escape', 'disconnect'])
async def test_model_configuration_ack_and_cancellation_fence_queued_turns(
  outcome: str,
) -> None:
  async with _running() as (client, service, _, _):
    candidate = agent_pb2.InferenceConfig(
      api='anthropic-oauth', model='candidate'
    )
    previous = agent_pb2.InferenceConfig(
      api='anthropic-messages', model='previous'
    )
    operation = asyncio.create_task(
      client.select_claude_model(
        candidate, previous, accept=lambda: outcome != 'escape'
      )
    )
    request = await asyncio.wait_for(service.requests.get(), 3)
    assert request.set_inference_config.claude_session == _BINDING.to_proto()
    client.enqueue(agent_pb2.AgentMessage(id='queued-turn'))
    assert service.requests.empty()
    service.responses.put_nowait(_ack('unrelated'))
    await asyncio.sleep(0.02)
    assert not operation.done()
    if outcome == 'disconnect':
      await client.close()
      with pytest.raises(claude_session.ControlError):
        await operation
      assert service.requests.empty()
      return
    service.responses.put_nowait(
      _nack(request.message_id)
      if outcome == 'nack'
      else _ack(request.message_id)
    )
    if outcome == 'escape':
      restore = await asyncio.wait_for(service.requests.get(), 3)
      assert restore.set_inference_config.model == 'previous'
      assert not operation.done()
      assert service.requests.empty()
      service.responses.put_nowait(_ack(restore.message_id))
    if outcome == 'nack':
      with pytest.raises(claude_session.ControlError):
        await operation
    else:
      assert await operation == (outcome == 'ack')
    queued = await asyncio.wait_for(service.requests.get(), 3)
    assert queued.message_id == 'queued-turn'


@pytest.mark.parametrize(
  'corruption', ['version', 'identity', 'profile', 'conversation']
)
def test_invalid_native_snapshot_is_rejected(corruption: str) -> None:
  state = _state(_BINDING).claude_session_state
  if corruption == 'version':
    state.binding.schema_version = 2
  elif corruption == 'identity':
    state.binding.identity.account_id = 'invalid'
  elif corruption == 'profile':
    state.binding.compatibility_profile = '\x1b[2J'
  else:
    state.conversation_id = 'other'
  with pytest.raises(claude_auth.ProtocolError):
    claude_session.Snapshot.parse(state, 'conversation')


@pytest.mark.asyncio
@pytest.mark.parametrize('bound', [False, True])
async def test_development_native_attachment_nack_history_and_reconnect(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  bound: bool,
) -> None:
  executable = os.environ.get('MIRROR_TEST_NATIVE_WORKER')
  if not executable:
    pytest.skip(
      'Set MIRROR_TEST_NATIVE_WORKER for native development integration'
    )
  monkeypatch.setenv('HOME', str(tmp_path))
  for key in (
    'ANTHROPIC_API_KEY',
    'ANTHROPIC_AUTH_TOKEN',
    'ANTHROPIC_BASE_URL',
    'MIRROR_CODEX_AUTH_PATH',
  ):
    monkeypatch.delenv(key, raising=False)
  async with claude_auth.NativeCommand([executable], 'status') as command:
    status = await command.read_event()
    assert isinstance(status, claude_auth.Completed)
    native_binding = claude_session.Binding(
      **_IDENTITY, compatibility_profile=status.compatibility_profile
    )
  database = tmp_path / 'native.db'
  config = {
    'conversation_id': 'conversation',
    'model': 'claude-opus-4-8',
    'api': 'anthropic-oauth',
    'database': str(database),
    'create_session': True,
    'tools': [],
    'include_environment': False,
    'checkpoints_enabled': False,
  }
  events = []
  errors = mock.AsyncMock()

  def client(create: bool) -> rpc_client.Client:
    transport = local_rpc_client.Transport(
      [executable],
      dict(config, create_session=create),
      workspace=tmp_path,
      log_dir=tmp_path / 'logs',
    )
    return rpc_client.Client(
      transport,
      'conversation',
      on_message=events.append,
      on_error=errors,
      inference_config=agent_pb2.InferenceConfig(
        api='anthropic-oauth', model='claude-opus-4-8'
      ),
    )

  first = client(True)
  try:
    await asyncio.wait_for(
      first.start(replay=True, wait_for_attachment=True), 10
    )
    assert first.claude_snapshot == claude_session.Snapshot(
      'conversation', None
    )
    assert any(e.input_nack.HasField('claude_error') for e in events)
  finally:
    await first.close()
  # Seed only nonsecret durable history/binding to exercise resume without a grant.
  history = agent_pb2.AgentMessage(
    id='retained',
    chat=agent_pb2.ChatRecord(
      user=agent_pb2.UserMessage(content='Synthetic retained history'),
      success=True,
    ),
  )
  with sqlite3.connect(database) as db:
    db.execute(
      'INSERT INTO react_entry(session_id, sequence, entry_id, payload) VALUES (?, ?, ?, ?)',
      (
        'conversation',
        db.execute(
          "SELECT COALESCE(MAX(sequence), 0) + 1 FROM react_entry WHERE session_id = 'conversation'"
        ).fetchone()[0],
        history.id,
        history.SerializeToString(),
      ),
    )
    if bound:
      db.execute(
        'UPDATE react_session SET claude_session = ? WHERE id = ?',
        (
          json.dumps({
            'schema_version': 1,
            'identity': _IDENTITY,
            'compatibility_profile': native_binding.compatibility_profile,
          }),
          'conversation',
        ),
      )
  events.clear()
  resumed = client(False)
  try:
    await asyncio.wait_for(
      resumed.start(replay=True, wait_for_attachment=True), 10
    )
    assert resumed.claude_snapshot == claude_session.Snapshot(
      'conversation', native_binding if bound else None
    )
    assert any(e.agent_message.id == 'retained' for e in events)
    assert any(e.input_nack.HasField('claude_error') for e in events)
    with pytest.raises(claude_session.ControlError, match='login|Sign|sign'):
      await resumed.reconnect_claude(_OTHER)
    assert resumed.claude_snapshot == claude_session.Snapshot(
      'conversation', native_binding if bound else None
    )
    assert any(entry.id == 'retained' for entry in await resumed.messages())
    errors.assert_not_called()
  finally:
    await resumed.close()
