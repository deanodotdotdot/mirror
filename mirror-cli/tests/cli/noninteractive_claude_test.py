"""Native incomplete responses and typed terminal errors never become exec success."""

import asyncio

import pytest

from mirror.cli import noninteractive
from mirror.protocol import agent_pb2


def _partial() -> agent_pb2.SessionMessage:
  return agent_pb2.SessionMessage(
    stream_final=True,
    agent_message=agent_pb2.AgentMessage(
      inference=agent_pb2.InferenceConfig(
        api='anthropic-oauth', model='claude-opus-4-8'
      ),
      chat=agent_pb2.ChatRecord(
        assistant=agent_pb2.AssistantMessage(content='Unfinished answer'),
        success=False,
        truncated=True,
        display='Incomplete tool call read: {"path":',
      ),
    ),
  )


def _error() -> agent_pb2.SessionMessage:
  return agent_pb2.SessionMessage(
    runtime_delta=agent_pb2.RuntimeDelta(
      turn_error=agent_pb2.TurnError(
        message='Claude response was interrupted',
        claude_error=agent_pb2.ClaudeErrorDetails(
          schema_version=1,
          kind='partial_stream_error',
          retry_disposition='terminal',
          recovery='none',
        ),
      )
    )
  )


@pytest.mark.parametrize('typed_error', [False, True])
def test_incomplete_native_stream_final_is_never_an_exec_answer(
  typed_error: bool,
) -> None:
  async def answer() -> str:
    events: asyncio.Queue[agent_pb2.SessionMessage | None] = asyncio.Queue()
    events.put_nowait(_partial())
    if typed_error:
      events.put_nowait(_error())
    completed = agent_pb2.SessionMessage()
    completed.runtime_delta.turn_completed.SetInParent()
    events.put_nowait(completed)
    return await noninteractive._answer(events)

  with pytest.raises(RuntimeError):
    asyncio.run(answer())


def test_exec_returns_nonzero_for_native_partial_then_typed_terminal_error(
  monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
  async def native_events(*args: object, **kwargs: object) -> str:
    del args, kwargs
    events: asyncio.Queue[agent_pb2.SessionMessage | None] = asyncio.Queue()
    events.put_nowait(_partial())
    events.put_nowait(_error())
    return await noninteractive._answer(events)

  monkeypatch.setattr(noninteractive, 'read_prompt', lambda _: 'task')
  monkeypatch.setattr(noninteractive, '_run', native_events)
  assert noninteractive.run(
    ['unused-worker'],
    {'conversation_id': 'synthetic-session', 'model': 'claude-opus-4-8'},
    'task',
    reasoning_effort='none',
  ) == 1
  captured = capsys.readouterr()
  assert 'mirror:' in captured.err
