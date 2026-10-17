"""Tests for the ACP stdio server over a fake agent worker."""

from __future__ import annotations

import asyncio
import json
import pathlib
from collections.abc import AsyncIterator, Mapping, Sequence
from typing import cast, override

import grpc
import pytest
import pytest_asyncio

from mirror.cli import acp, host_tools, rpc_client
from mirror.protocol import (
  agent_pb2,
  agent_pb2_grpc,
)


def _delta(**fields: object) -> agent_pb2.SessionMessage:
  """A runtime delta carrying one streamed field."""
  return agent_pb2.SessionMessage(
    runtime_delta=agent_pb2.RuntimeDelta(**fields)  # pyright: ignore[reportArgumentType]
  )


class _FakeAgent(agent_pb2_grpc.AgentRPCServicer):
  """Scripted worker: a thinking delta, an approved tool call, streamed text."""

  def __init__(self) -> None:
    self.approvals: list[agent_pb2.HITLResponse] = []
    self.interrupts = 0
    self.user_messages: list[str] = []
    self.configs: list[agent_pb2.InferenceConfig] = []
    self.approval_in_flight = False

  @override
  async def Session(  # noqa: N802
    self,
    request_iterator: AsyncIterator[agent_pb2.ClientMessage],
    context: grpc.aio.ServicerContext[
      agent_pb2.ClientMessage, agent_pb2.SessionMessage
    ],
  ) -> AsyncIterator[agent_pb2.SessionMessage]:
    async for request in request_iterator:
      payload = request.WhichOneof('payload')
      if payload == 'set_inference_config':
        self.configs.append(request.set_inference_config)
        yield agent_pb2.SessionMessage(
          input_ack=agent_pb2.InputAck(message_id=request.message_id)
        )
        continue
      if payload == 'interrupt':
        self.interrupts += 1
        if self.approval_in_flight:
          # The approval request crossed the interrupt on the wire.
          self.approval_in_flight = False
          yield agent_pb2.SessionMessage(
            hitl_request=agent_pb2.HITLRequest(
              call=agent_pb2.ToolCall(
                id='call-3',
                type='function',
                function=agent_pb2.FunctionCall(name='bash', arguments='{}'),
              )
            )
          )
        # Like the worker, an interrupted turn still ends with a completion.
        yield _delta(turn_interrupted=agent_pb2.RuntimeDelta().turn_interrupted)
        yield _delta(turn_completed=agent_pb2.RuntimeDelta().turn_completed)
        continue
      if payload == 'hitl_response':
        self.approvals.append(request.hitl_response)
        yield agent_pb2.SessionMessage(
          hitl_resolved=agent_pb2.HITLResolved(
            call_id=request.hitl_response.call_id,
            approved=request.hitl_response.approved,
          )
        )
        if not request.hitl_response.approved:
          yield _delta(turn_completed=agent_pb2.RuntimeDelta().turn_completed)
          continue
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='tool-1',
            chat=agent_pb2.ChatRecord(
              tool=agent_pb2.ToolMessage(
                tool_call_id='call-1', content='README.md\nmain.go', name='bash'
              ),
              success=True,
            ),
          )
        )
        for word in ('Hello ', 'world'):
          yield _delta(stream_content=agent_pb2.StreamContent(content=word))
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='assistant-2',
            chat=agent_pb2.ChatRecord(
              assistant=agent_pb2.AssistantMessage(content='Hello world'),
              usage=agent_pb2.Usage(prompt_tokens=30, completion_tokens=4),
              success=True,
            ),
            inference=agent_pb2.InferenceConfig(model='reflection/beam'),
          ),
          stream_final=True,
        )
        yield _delta(turn_completed=agent_pb2.RuntimeDelta().turn_completed)
        continue
      if payload != 'message':
        continue
      content = request.message.chat.user.content
      self.user_messages.append(content)
      yield agent_pb2.SessionMessage(
        input_ack=agent_pb2.InputAck(message_id=request.message_id)
      )
      yield agent_pb2.SessionMessage(agent_message=request.message)
      if content.startswith('notice'):
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='notice-1',
            chat=agent_pb2.ChatRecord(
              assistant=agent_pb2.AssistantMessage(
                content='Model request failed; check credentials.'
              ),
              success=True,
            ),
          )
        )
        yield _delta(turn_completed=agent_pb2.RuntimeDelta().turn_completed)
        continue
      if content.startswith('unstreamed'):
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='assistant-3',
            chat=agent_pb2.ChatRecord(
              assistant=agent_pb2.AssistantMessage(content='Whole answer'),
              success=True,
            ),
            inference=agent_pb2.InferenceConfig(model='reflection/beam'),
          )
        )
        yield _delta(turn_completed=agent_pb2.RuntimeDelta().turn_completed)
        continue
      if content.startswith('hang'):
        # Stay mid-turn until interrupted.
        self.approval_in_flight = content.startswith('hang-approval')
        yield _delta(stream_content=agent_pb2.StreamContent(content='working'))
        continue
      if content.startswith('drop-during-approval'):
        yield agent_pb2.SessionMessage(
          hitl_request=agent_pb2.HITLRequest(
            call=agent_pb2.ToolCall(
              id='call-4',
              type='function',
              function=agent_pb2.FunctionCall(name='bash', arguments='{}'),
            )
          )
        )
        # The worker dies while the client is deciding.
        return
      if content.startswith('toolfail'):
        # A successful tool round, then the next model call fails and the
        # worker commits a notice instead of an answer.
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='assistant-4',
            chat=agent_pb2.ChatRecord(
              assistant=agent_pb2.AssistantMessage(
                tool_calls=[
                  agent_pb2.ToolCall(
                    id='call-2',
                    type='function',
                    function=agent_pb2.FunctionCall(
                      name='bash', arguments='{"command": "pwd"}'
                    ),
                  )
                ]
              ),
              success=True,
            ),
            inference=agent_pb2.InferenceConfig(model='reflection/beam'),
          )
        )
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='tool-2',
            chat=agent_pb2.ChatRecord(
              tool=agent_pb2.ToolMessage(
                tool_call_id='call-2', content='/repo', name='bash'
              ),
              success=True,
            ),
          )
        )
        yield agent_pb2.SessionMessage(
          agent_message=agent_pb2.AgentMessage(
            id='notice-2',
            chat=agent_pb2.ChatRecord(
              assistant=agent_pb2.AssistantMessage(
                content='Provider returned 429; giving up.'
              ),
              success=True,
            ),
          )
        )
        yield _delta(turn_completed=agent_pb2.RuntimeDelta().turn_completed)
        continue
      yield _delta(stream_reasoning=agent_pb2.StreamReasoning(reasoning='Hmm.'))
      call = agent_pb2.ToolCall(
        id='call-1',
        type='function',
        function=agent_pb2.FunctionCall(
          name='bash', arguments='{"command": "ls"}'
        ),
      )
      yield agent_pb2.SessionMessage(
        agent_message=agent_pb2.AgentMessage(
          id='assistant-1',
          chat=agent_pb2.ChatRecord(
            assistant=agent_pb2.AssistantMessage(tool_calls=[call]),
            usage=agent_pb2.Usage(prompt_tokens=20, completion_tokens=6),
            success=True,
          ),
          inference=agent_pb2.InferenceConfig(model='reflection/beam'),
        )
      )
      yield agent_pb2.SessionMessage(
        hitl_request=agent_pb2.HITLRequest(call=call)
      )


class _ChannelTransport:
  """A transport over an already running fake worker."""

  def __init__(self, port: int, ready: asyncio.Event | None = None) -> None:
    self._port = port
    self._ready = ready
    self._channel: grpc.aio.Channel | None = None
    self.closed = False

  async def connect(
    self, host_tools: Sequence[host_tools.ToolDefinition] = ()
  ) -> grpc.aio.Channel:
    del host_tools
    if self._ready is not None:
      # A slow worker start.
      await self._ready.wait()
    self._channel = grpc.aio.insecure_channel(f'127.0.0.1:{self._port}')
    return self._channel

  async def close(self) -> None:
    self.closed = True
    if self._channel is not None:
      await self._channel.close()
      self._channel = None


class _Lines:
  """Collect protocol output as decoded messages."""

  def __init__(self) -> None:
    self.messages: asyncio.Queue[dict[str, object]] = asyncio.Queue()

  def write(self, data: bytes) -> None:
    for line in data.decode('utf-8').splitlines():
      if line.strip():
        self.messages.put_nowait(json.loads(line))

  def flush(self) -> None:
    return None


class _Client:
  """Drive the server the way an ACP client does."""

  def __init__(
    self, reader: asyncio.StreamReader, output: _Lines, server: acp.Server
  ) -> None:
    self._reader = reader
    self._output = output
    self._next_id = 0
    self.server = server
    self.transports: list[_ChannelTransport] = []

  def inject_worker_events(
    self, session_id: str, events: Sequence[agent_pb2.SessionMessage | None]
  ) -> None:
    """Queue worker events as if they had arrived, `None` being a disconnect."""
    session = self.server._sessions[session_id]  # pyright: ignore[reportPrivateUsage]
    for event in events:
      session.events.put_nowait(event)
      if event is None:
        session.failed = True

  def request(self, method: str, params: dict[str, object]) -> int:
    self._next_id += 1
    self._write({
      'jsonrpc': '2.0',
      'id': self._next_id,
      'method': method,
      'params': params,
    })
    return self._next_id

  def notify(self, method: str, params: dict[str, object]) -> None:
    self._write({'jsonrpc': '2.0', 'method': method, 'params': params})

  def respond(self, request_id: object, result: dict[str, object]) -> None:
    self._write({'jsonrpc': '2.0', 'id': request_id, 'result': result})

  def close(self) -> None:
    self._reader.feed_eof()

  def request_then_cancel(self, session_id: str, text: str) -> int:
    """Deliver a prompt and its cancel in one read, as a fast client can."""
    self._next_id += 1
    lines = [
      {
        'jsonrpc': '2.0',
        'id': self._next_id,
        'method': 'session/prompt',
        'params': {
          'sessionId': session_id,
          'prompt': [{'type': 'text', 'text': text}],
        },
      },
      {
        'jsonrpc': '2.0',
        'method': 'session/cancel',
        'params': {'sessionId': session_id},
      },
    ]
    self._reader.feed_data(
      b''.join(json.dumps(line).encode('utf-8') + b'\n' for line in lines)
    )
    return self._next_id

  def cancel_then_respond(self, session_id: str, request_id: object) -> None:
    """Deliver a cancel and the permission answer in one read."""
    lines = [
      {
        'jsonrpc': '2.0',
        'method': 'session/cancel',
        'params': {'sessionId': session_id},
      },
      {
        'jsonrpc': '2.0',
        'id': request_id,
        'result': {'outcome': {'outcome': 'cancelled'}},
      },
    ]
    self._reader.feed_data(
      b''.join(json.dumps(line).encode('utf-8') + b'\n' for line in lines)
    )

  def _write(self, message: dict[str, object]) -> None:
    self._reader.feed_data(json.dumps(message).encode('utf-8') + b'\n')

  async def next(self) -> dict[str, object]:
    return await asyncio.wait_for(self._output.messages.get(), timeout=5)

  async def wait_response(
    self, request_id: int
  ) -> tuple[dict[str, object], list[dict[str, object]]]:
    """Return the response and the server messages that preceded it."""
    preceding: list[dict[str, object]] = []
    while True:
      message = await self.next()
      if message.get('id') == request_id and 'method' not in message:
        return message, preceding
      preceding.append(message)


@pytest_asyncio.fixture
async def fake_agent() -> AsyncIterator[tuple[_FakeAgent, int]]:
  agent = _FakeAgent()
  server = grpc.aio.server()
  agent_pb2_grpc.add_AgentRPCServicer_to_server(  # pyright: ignore[reportUnknownMemberType]
    agent, server
  )
  port = server.add_insecure_port('127.0.0.1:0')
  await server.start()
  try:
    yield agent, port
  finally:
    await server.stop(None)


async def _start(
  port: int,
  tmp_path: pathlib.Path,
  second_worker_ready: asyncio.Event | None = None,
) -> tuple[_Client, asyncio.Task[None], list[Mapping[str, object]]]:
  configs: list[Mapping[str, object]] = []
  transports: list[_ChannelTransport] = []

  def worker_config(conversation_id: str) -> Mapping[str, object]:
    config = {
      'conversation_id': conversation_id,
      'model': 'reflection/beam',
      'approval_mode': 'approve',
    }
    configs.append(config)
    return config

  def transport(
    config: Mapping[str, object],
    workspace: pathlib.Path,
    conversation_id: str,
    on_message: rpc_client.MessageHandler,
  ) -> rpc_client.Transport:
    del config, workspace, conversation_id, on_message
    transport = _ChannelTransport(
      port, second_worker_ready if len(configs) == 2 else None
    )
    transports.append(transport)
    return transport

  server = acp.Server(
    worker_config=worker_config,
    transport=transport,
    reasoning_effort='high',
    record_history=False,
  )
  reader = asyncio.StreamReader()
  output = _Lines()
  task = asyncio.create_task(server.serve(reader, output))
  client = _Client(reader, output, server)
  client.transports = transports

  response, _ = await client.wait_response(
    client.request('initialize', {'protocolVersion': 1})
  )
  assert response['result'] == {
    'protocolVersion': 1,
    'agentCapabilities': {
      'loadSession': False,
      'promptCapabilities': {
        'image': False,
        'audio': False,
        'embeddedContext': True,
      },
    },
    'authMethods': [],
  }
  response, _ = await client.wait_response(
    client.request('session/new', {'cwd': str(tmp_path), 'mcpServers': []})
  )
  result = response['result']
  assert isinstance(result, dict)
  assert result['sessionId'] == configs[0]['conversation_id']
  assert result['models'] == {
    'availableModels': [
      {'modelId': 'reflection/beam', 'name': 'reflection/beam'}
    ],
    'currentModelId': 'reflection/beam',
  }
  return client, task, configs


async def _finish(client: _Client, task: asyncio.Task[None]) -> None:
  client.close()
  await asyncio.wait_for(task, timeout=5)


def _content_text(update: dict[str, object]) -> object:
  """The text of an update's single content block, if it has one."""
  content = update.get('content')
  if not isinstance(content, dict):
    return None
  return cast(dict[str, object], content).get('text')


def _chunk_texts(messages: Sequence[dict[str, object]]) -> list[object]:
  """The text of every streamed chunk among the server's notifications."""
  chunks: list[object] = []
  for message in messages:
    if message.get('method') != 'session/update':
      continue
    params = cast(dict[str, object], message['params'])
    update = cast(dict[str, object], params['update'])
    content = cast(dict[str, object], update['content'])
    chunks.append(content['text'])
  return chunks


@pytest.mark.asyncio
async def test_prompt_streams_turn_and_forwards_permission(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])

  prompt_id = client.request(
    'session/prompt',
    {
      'sessionId': session_id,
      'prompt': [
        {'type': 'text', 'text': 'list files'},
        {
          'type': 'resource',
          'resource': {'uri': 'file:///x.go', 'text': 'package x'},
        },
      ],
    },
  )
  updates: list[dict[str, object]] = []
  while True:
    message = await client.next()
    if message.get('method') == 'session/request_permission':
      params = cast(dict[str, object], message['params'])
      assert params['sessionId'] == session_id
      tool_call = cast(dict[str, object], params['toolCall'])
      assert tool_call['toolCallId'] == 'call-1'
      assert tool_call['rawInput'] == {'command': 'ls'}
      client.respond(
        message['id'],
        {'outcome': {'outcome': 'selected', 'optionId': 'allow'}},
      )
      continue
    if message.get('id') == prompt_id:
      break
    params = cast(dict[str, object], message['params'])
    assert params['sessionId'] == session_id
    updates.append(cast(dict[str, object], params['update']))

  assert message['result'] == {
    'stopReason': 'end_turn',
    'usage': {'inputTokens': 50, 'outputTokens': 10, 'totalTokens': 60},
  }
  assert agent.user_messages == [
    'list files\n\n<file uri="file:///x.go">\npackage x\n</file>'
  ]
  assert [approval.approved for approval in agent.approvals] == [True]
  assert agent.configs[0].model == 'reflection/beam'
  assert agent.configs[0].reasoning_effort == 'high'
  assert [
    (
      update['sessionUpdate'],
      update.get('toolCallId'),
      update.get('status'),
      _content_text(update),
    )
    for update in updates
  ] == [
    ('agent_thought_chunk', None, None, 'Hmm.'),
    ('tool_call', 'call-1', 'in_progress', None),
    ('tool_call_update', 'call-1', 'completed', None),
    ('agent_message_chunk', None, None, 'Hello '),
    ('agent_message_chunk', None, None, 'world'),
  ]
  tool_update = updates[2]
  assert tool_update['content'] == [
    {
      'type': 'content',
      'content': {'type': 'text', 'text': 'README.md\nmain.go'},
    }
  ]
  await _finish(client, task)


@pytest.mark.asyncio
async def test_rejected_permission_denies_the_call(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'go'}]},
  )
  while True:
    message = await client.next()
    if message.get('method') == 'session/request_permission':
      client.respond(message['id'], {'outcome': {'outcome': 'cancelled'}})
      continue
    if message.get('id') == prompt_id:
      break
  assert message['result'] == {
    'stopReason': 'end_turn',
    'usage': {'inputTokens': 20, 'outputTokens': 6, 'totalTokens': 26},
  }
  assert [approval.approved for approval in agent.approvals] == [False]
  await _finish(client, task)


@pytest.mark.asyncio
async def test_cancel_interrupts_the_running_turn(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'hang'}]},
  )
  while True:
    message = await client.next()
    if 'working' in json.dumps(message):
      break
  client.notify('session/cancel', {'sessionId': session_id})
  response, _ = await client.wait_response(prompt_id)
  assert response['result'] == {'stopReason': 'cancelled'}
  assert agent.interrupts == 1
  # The interrupted turn's completion must not end the next prompt early.
  response, preceding = await client.wait_response(
    client.request(
      'session/prompt',
      {
        'sessionId': session_id,
        'prompt': [{'type': 'text', 'text': 'unstreamed'}],
      },
    )
  )
  assert response['result'] == {'stopReason': 'end_turn'}
  assert _chunk_texts(preceding) == ['Whole answer']
  await _finish(client, task)


@pytest.mark.asyncio
async def test_cancel_before_the_turn_starts_is_not_lost(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request_then_cancel(session_id, 'list files')
  response, _ = await client.wait_response(prompt_id)
  assert response['result'] == {'stopReason': 'cancelled'}
  assert agent.user_messages == []
  assert agent.interrupts == 0
  await _finish(client, task)


@pytest.mark.asyncio
async def test_cancel_while_permission_is_pending(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'go'}]},
  )
  while True:
    message = await client.next()
    if message.get('method') == 'session/request_permission':
      break
  # The client dismisses its approval card without answering.
  client.notify('session/cancel', {'sessionId': session_id})
  response, _ = await client.wait_response(prompt_id)
  assert response['result'] == {
    'stopReason': 'cancelled',
    'usage': {'inputTokens': 20, 'outputTokens': 6, 'totalTokens': 26},
  }
  assert agent.interrupts == 1
  assert agent.approvals == []
  # A late answer to the dismissed request is ignored, and the session works.
  client.respond(
    message['id'], {'outcome': {'outcome': 'selected', 'optionId': 'allow'}}
  )
  response, preceding = await client.wait_response(
    client.request(
      'session/prompt',
      {
        'sessionId': session_id,
        'prompt': [{'type': 'text', 'text': 'unstreamed'}],
      },
    )
  )
  assert response['result'] == {'stopReason': 'end_turn'}
  assert _chunk_texts(preceding) == ['Whole answer']
  assert agent.approvals == []
  await _finish(client, task)


@pytest.mark.asyncio
async def test_cancel_and_permission_answer_in_one_read(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'go'}]},
  )
  while True:
    message = await client.next()
    if message.get('method') == 'session/request_permission':
      break
  client.cancel_then_respond(session_id, message['id'])
  response, _ = await client.wait_response(prompt_id)
  assert response['result'] == {
    'stopReason': 'cancelled',
    'usage': {'inputTokens': 20, 'outputTokens': 6, 'totalTokens': 26},
  }
  assert agent.interrupts == 1
  assert agent.approvals == []
  # The server survived the already-settled answer and still serves.
  response, preceding = await client.wait_response(
    client.request(
      'session/prompt',
      {
        'sessionId': session_id,
        'prompt': [{'type': 'text', 'text': 'unstreamed'}],
      },
    )
  )
  assert response['result'] == {'stopReason': 'end_turn'}
  assert _chunk_texts(preceding) == ['Whole answer']
  await _finish(client, task)


@pytest.mark.asyncio
async def test_cancel_before_a_queued_permission_request(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {
      'sessionId': session_id,
      'prompt': [{'type': 'text', 'text': 'hang-approval'}],
    },
  )
  while True:
    message = await client.next()
    if 'working' in json.dumps(message):
      break
  client.notify('session/cancel', {'sessionId': session_id})
  response, preceding = await client.wait_response(prompt_id)
  assert response['result'] == {'stopReason': 'cancelled'}
  # The approval that arrived after the cancel was never put to the client.
  assert [m.get('method') for m in preceding] == []
  assert agent.interrupts == 1
  assert agent.approvals == []
  await _finish(client, task)


@pytest.mark.asyncio
async def test_worker_loss_during_approval_fails_prompt_and_session(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  _, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {
      'sessionId': session_id,
      'prompt': [{'type': 'text', 'text': 'drop-during-approval'}],
    },
  )
  # The permission request is outstanding when the worker goes away; the
  # client never answers it.
  response, preceding = await client.wait_response(prompt_id)
  assert [m.get('method') for m in preceding] == ['session/request_permission']
  assert response['error'] == {
    'code': -32603,
    'message': 'agent connection failed; check provider configuration',
  }
  response, _ = await client.wait_response(
    client.request(
      'session/prompt',
      {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'again'}]},
    )
  )
  assert response['error'] == {
    'code': -32603,
    'message': 'agent connection failed; start a new session',
  }
  await _finish(client, task)


@pytest.mark.asyncio
async def test_queued_approval_after_worker_loss_is_not_asked(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  _, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'hang'}]},
  )
  while True:
    message = await client.next()
    if 'working' in json.dumps(message):
      break
  # The worker asks for an approval and dies before the turn reads it.
  client.inject_worker_events(
    session_id,
    [
      agent_pb2.SessionMessage(
        hitl_request=agent_pb2.HITLRequest(
          call=agent_pb2.ToolCall(
            id='call-5',
            type='function',
            function=agent_pb2.FunctionCall(name='bash', arguments='{}'),
          )
        )
      ),
      None,
    ],
  )
  response, preceding = await client.wait_response(prompt_id)
  assert [m.get('method') for m in preceding] == []
  assert response['error'] == {
    'code': -32603,
    'message': 'agent connection failed; check provider configuration',
  }
  await _finish(client, task)


@pytest.mark.asyncio
async def test_shutdown_during_session_start_closes_the_worker(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  _, port = fake_agent
  ready = asyncio.Event()
  client, task, configs = await _start(port, tmp_path, ready)
  new_id = client.request(
    'session/new', {'cwd': str(tmp_path), 'mcpServers': []}
  )

  async def second_worker_starting() -> None:
    while len(configs) < 2:
      await asyncio.sleep(0)

  await asyncio.wait_for(second_worker_starting(), timeout=5)
  # The ACP client goes away while the second worker is still starting.
  await _finish(client, task)
  assert [transport.closed for transport in client.transports] == [True, True]
  # No response was written for the abandoned request.
  leftover: list[dict[str, object]] = []
  output = client._output  # pyright: ignore[reportPrivateUsage]
  while not output.messages.empty():
    leftover.append(output.messages.get_nowait())
  assert all(m.get('id') != new_id for m in leftover)


@pytest.mark.asyncio
async def test_slow_session_start_does_not_block_other_sessions(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  agent, port = fake_agent
  ready = asyncio.Event()
  client, task, configs = await _start(port, tmp_path, ready)
  session_id = str(configs[0]['conversation_id'])
  prompt_id = client.request(
    'session/prompt',
    {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'hang'}]},
  )
  while True:
    message = await client.next()
    if 'working' in json.dumps(message):
      break
  # A second session whose worker is slow to start...
  new_id = client.request(
    'session/new', {'cwd': str(tmp_path), 'mcpServers': []}
  )

  async def second_worker_starting() -> None:
    while len(configs) < 2:
      await asyncio.sleep(0)

  await asyncio.wait_for(second_worker_starting(), timeout=5)
  # ...must not keep the first session's cancel from being served.
  client.notify('session/cancel', {'sessionId': session_id})
  response, preceding = await client.wait_response(prompt_id)
  assert response['result'] == {'stopReason': 'cancelled'}
  assert agent.interrupts == 1
  assert all(m.get('id') != new_id for m in preceding)
  ready.set()
  response, _ = await client.wait_response(new_id)
  result = response['result']
  assert isinstance(result, dict)
  assert result['sessionId'] == configs[1]['conversation_id']
  await _finish(client, task)


@pytest.mark.asyncio
async def test_failure_notice_after_tool_round_fails_the_prompt(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  _, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  response, preceding = await client.wait_response(
    client.request(
      'session/prompt',
      {
        'sessionId': session_id,
        'prompt': [{'type': 'text', 'text': 'toolfail'}],
      },
    )
  )
  assert response['error'] == {
    'code': -32603,
    'message': 'Provider returned 429; giving up.',
  }
  updates = [
    cast(dict[str, object], cast(dict[str, object], m['params'])['update'])
    for m in preceding
    if m.get('method') == 'session/update'
  ]
  assert [(u['sessionUpdate'], u.get('status')) for u in updates] == [
    ('tool_call', 'in_progress'),
    ('tool_call_update', 'completed'),
  ]
  await _finish(client, task)


def _raw_input(arguments: str) -> object:
  """The `rawInput` of the tool card for a call with these arguments."""
  updates, _ = acp.Translator().translate(
    agent_pb2.SessionMessage(
      agent_message=agent_pb2.AgentMessage(
        id='assistant-6',
        chat=agent_pb2.ChatRecord(
          assistant=agent_pb2.AssistantMessage(
            tool_calls=[
              agent_pb2.ToolCall(
                id='call-6',
                type='function',
                function=agent_pb2.FunctionCall(
                  name='bash', arguments=arguments
                ),
              )
            ]
          ),
          success=True,
        ),
        inference=agent_pb2.InferenceConfig(model='reflection/beam'),
      )
    )
  )
  return updates[0]['rawInput']


def test_tool_call_raw_input_preserves_empty_and_invalid_arguments() -> None:
  assert _raw_input('{}') == {}
  assert _raw_input('') == {}
  assert _raw_input('{"command": "ls"}') == {'command': 'ls'}
  assert _raw_input('[1, 2]') == {'arguments': [1, 2]}
  assert _raw_input('not json') == {'arguments': 'not json'}


def test_streamed_refusal_is_not_repeated() -> None:
  translator = acp.Translator()
  updates, outcome = translator.translate(
    agent_pb2.SessionMessage(
      runtime_delta=agent_pb2.RuntimeDelta(
        stream_refusal=agent_pb2.StreamRefusal(refusal='No.')
      )
    )
  )
  assert outcome is None
  assert [u['content'] for u in updates] == [{'type': 'text', 'text': 'No.'}]
  updates, outcome = translator.translate(
    agent_pb2.SessionMessage(
      agent_message=agent_pb2.AgentMessage(
        id='assistant-5',
        chat=agent_pb2.ChatRecord(
          assistant=agent_pb2.AssistantMessage(refusal='No.'), success=True
        ),
        inference=agent_pb2.InferenceConfig(model='reflection/beam'),
      )
    )
  )
  assert (updates, outcome) == ([], None)


@pytest.mark.asyncio
async def test_rejects_unknown_sessions_methods_and_bad_lines(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  _, port = fake_agent
  client, task, _ = await _start(port, tmp_path)
  response, _ = await client.wait_response(
    client.request(
      'session/prompt',
      {'sessionId': 'nope', 'prompt': [{'type': 'text', 'text': 'x'}]},
    )
  )
  assert response['error'] == {'code': -32602, 'message': 'unknown session'}
  response, _ = await client.wait_response(
    client.request('session/load', {'sessionId': 'nope'})
  )
  error = response['error']
  assert isinstance(error, dict)
  assert error['code'] == -32601
  client._reader.feed_data(b'not json\n')  # pyright: ignore[reportPrivateUsage]
  message = await client.next()
  error = message['error']
  assert isinstance(error, dict)
  assert error['code'] == -32700
  await _finish(client, task)


def test_prompt_text_and_tool_kinds() -> None:
  assert acp.prompt_text([
    {'type': 'text', 'text': 'a'},
    {'type': 'resource_link', 'uri': 'file:///b', 'name': 'b'},
    {'type': 'image', 'data': 'x'},
    {'type': 'text', 'text': ''},
  ]) == (
    'a\n\nReferenced resource: b (file:///b)\n\n'
    '[image attachment omitted: not supported by mirror acp]'
  )
  assert acp.tool_kind('bash') == 'execute'
  assert acp.tool_kind('Read') == 'read'
  assert acp.tool_kind('mirror_memory') == 'other'


@pytest.mark.asyncio
async def test_notice_fails_prompt_and_unstreamed_answer_is_forwarded(
  fake_agent: tuple[_FakeAgent, int], tmp_path: pathlib.Path
) -> None:
  _, port = fake_agent
  client, task, configs = await _start(port, tmp_path)
  session_id = str(configs[0]['conversation_id'])
  response, _ = await client.wait_response(
    client.request(
      'session/prompt',
      {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': 'notice'}]},
    )
  )
  assert response['error'] == {
    'code': -32603,
    'message': 'Model request failed; check credentials.',
  }
  response, preceding = await client.wait_response(
    client.request(
      'session/prompt',
      {
        'sessionId': session_id,
        'prompt': [{'type': 'text', 'text': 'unstreamed'}],
      },
    )
  )
  assert response['result'] == {'stopReason': 'end_turn'}
  assert _chunk_texts(preceding) == ['Whole answer']
  await _finish(client, task)


@pytest.mark.asyncio
@pytest.mark.parametrize('compactor', [False, True])
async def test_acp_direct_session_rejects_subscription_before_worker(
  tmp_path: pathlib.Path,
  compactor: bool,
) -> None:
  from unittest import mock

  config = {
    'model': 'main',
    'api': 'anthropic-messages' if compactor else 'anthropic-oauth',
  }
  if compactor:
    config.update(
      compaction_model='compact', compaction_model_api='anthropic-oauth'
    )
  transport = mock.Mock(
    side_effect=AssertionError('excluded worker must not start')
  )
  server = acp.Server(
    worker_config=lambda _: config,
    transport=transport,
    reasoning_effort='none',
    record_history=False,
  )
  with pytest.raises(acp.RPCError, match='not supported'):
    await server._new_session({'cwd': str(tmp_path)})  # pyright: ignore[reportPrivateUsage]
  transport.assert_not_called()
