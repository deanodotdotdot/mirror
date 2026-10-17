"""Nonsecret, native-owned conversation identity; never workspace preferences."""

from __future__ import annotations

import dataclasses

from mirror.protocol import agent_pb2

from mirror.cli import claude_auth, claude_errors


@dataclasses.dataclass(frozen=True)
class Binding:
  account_id: str
  organization_id: str
  compatibility_profile: str

  @property
  def identity(self) -> dict[str, str]:
    return {
      'account_id': self.account_id,
      'organization_id': self.organization_id,
    }

  @classmethod
  def parse(cls, value: agent_pb2.ClaudeSessionBinding) -> Binding:
    if value.schema_version != 1 or not value.HasField('identity'):
      raise claude_auth.ProtocolError()
    identity = claude_auth.parse_identity({
      'account_id': value.identity.account_id,
      'organization_id': value.identity.organization_id,
    })
    if identity is None:
      raise claude_auth.ProtocolError()
    return cls(
      identity['account_id'],
      identity['organization_id'],
      claude_auth.compatibility_profile(value.compatibility_profile),
    )

  def to_proto(self) -> agent_pb2.ClaudeSessionBinding:
    return agent_pb2.ClaudeSessionBinding(
      schema_version=1,
      identity=agent_pb2.ClaudeIdentityBinding(**self.identity),
      compatibility_profile=self.compatibility_profile,
    )


@dataclasses.dataclass(frozen=True)
class Snapshot:
  """Presence means known; a null binding explicitly means unbound."""

  conversation_id: str
  binding: Binding | None

  @classmethod
  def parse(
    cls, value: agent_pb2.ClaudeSessionState, conversation_id: str
  ) -> Snapshot:
    if value.conversation_id != conversation_id:
      raise claude_auth.ProtocolError()
    if value.HasField('message_id') and not value.message_id:
      raise claude_auth.ProtocolError()
    return cls(
      conversation_id,
      Binding.parse(value.binding) if value.HasField('binding') else None,
    )


class ControlError(RuntimeError):
  """Only reviewed text; unknown outcomes require reattachment before retry."""

  def __init__(self, message: str, *, unknown: bool = False) -> None:
    super().__init__(message)
    self.unknown = unknown


def nack_text(value: agent_pb2.InputNack) -> str:
  if value.HasField('claude_error'):
    return claude_errors.text(value.claude_error)
  return {
    agent_pb2.INPUT_NACK_REASON_CONVERSATION_BUSY: 'The conversation is busy. Finish or interrupt its work, then retry /reconnect claude.',
    agent_pb2.INPUT_NACK_REASON_MESSAGE_ID_CONFLICT: 'The request identity conflicts with an earlier operation. Retry with a new request.',
    agent_pb2.INPUT_NACK_REASON_SESSION_MISMATCH: 'The conversation changed. Reopen the intended conversation before retrying.',
  }.get(
    value.reason,
    'The native worker rejected the Claude operation. Check the selected account and configuration.',
  )
