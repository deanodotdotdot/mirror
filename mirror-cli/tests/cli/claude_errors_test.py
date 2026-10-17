"""Typed Claude errors preserve recovery without exposing raw provider text."""

from __future__ import annotations

import asyncio
import datetime

import pytest
from google.protobuf import empty_pb2
from mirror.protocol import agent_pb2

from mirror.cli import claude_errors, noninteractive


def _error(
  kind: str = 'login_required', recovery: str = 'login'
) -> agent_pb2.ClaudeErrorDetails:
  return agent_pb2.ClaudeErrorDetails(
    schema_version=1,
    kind=kind,
    recovery=recovery,
    retry_disposition='terminal',
    compatibility_profile='synthetic-fixture-v1',
  )


def test_typed_error_recovery_and_compatibility() -> None:
  assert 'mirror auth login claude' in claude_errors.text(_error())
  assert '/reconnect claude' in claude_errors.text(
    _error('account_changed', 'reconnect')
  )
  assert 'synthetic-fixture-v1' in claude_errors.text(
    _error('compatibility_error', 'upgrade')
  )


def test_typed_limit_information_keeps_unknown_and_local_time() -> None:
  error = _error('subscription_limited', 'retry_later')
  error.extra_usage = 'unknown'
  error.limit_scope = 'shared'
  error.resets_at.FromDatetime(
    datetime.datetime(2026, 10, 17, 19, tzinfo=datetime.UTC)
  )
  result = claude_errors.text(error)
  assert 'Account extra usage: unknown' in result
  assert 'Reported limit scope: shared' in result
  assert 'UTC' in result


@pytest.mark.parametrize(
  'field', ['kind', 'recovery', 'configuration', 'provider_code']
)
def test_unrecognized_error_fields_never_echo(field: str) -> None:
  error = _error()
  setattr(error, field, 'SYNTHETIC_PRIVATE_CANARY')
  assert 'SYNTHETIC_PRIVATE_CANARY' not in claude_errors.text(error)


def test_invalid_typed_error_is_safe() -> None:
  error = _error()
  error.compatibility_profile = 'SYNTHETIC_PRIVATE_CANARY\n'
  assert 'SYNTHETIC_PRIVATE_CANARY' not in claude_errors.text(error)
  assert 'unsupported error contract' in claude_errors.text(error)


@pytest.mark.asyncio
async def test_exec_typed_failure_cannot_become_success_or_print_raw_error() -> (
  None
):
  events: asyncio.Queue[agent_pb2.SessionMessage | None] = asyncio.Queue()
  events.put_nowait(
    agent_pb2.SessionMessage(
      runtime_delta=agent_pb2.RuntimeDelta(
        turn_error=agent_pb2.TurnError(
          message='SYNTHETIC_PRIVATE_CANARY',
          claude_error=_error('partial_stream_error', 'none'),
        )
      )
    )
  )
  events.put_nowait(
    agent_pb2.SessionMessage(
      runtime_delta=agent_pb2.RuntimeDelta(turn_completed=empty_pb2.Empty())
    )
  )
  with pytest.raises(RuntimeError, match='output is incomplete') as failure:
    await noninteractive._answer(events)
  assert 'SYNTHETIC_PRIVATE_CANARY' not in str(failure.value)
