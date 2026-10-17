"""Render only the typed, validated Claude error contract from worker events."""

from __future__ import annotations

from mirror.protocol import agent_pb2

from mirror import claude_commands
from mirror.cli import claude_auth


def text(value: agent_pb2.ClaudeErrorDetails) -> str:
  """Raw worker/provider messages are never used when typed details exist."""
  try:
    details: dict[str, object] = {}
    for field in (
      'http_status',
      'provider_code',
      'extra_usage',
      'configuration',
      'limit_scope',
    ):
      if value.HasField(field):
        details[field] = getattr(value, field)
    for field in ('retry_at', 'resets_at'):
      if value.HasField(field):
        details[field] = getattr(value, field).ToJsonString()
    for field in ('expected_identity', 'current_identity'):
      if value.HasField(field):
        identity = getattr(value, field)
        details[field] = {
          'account_id': identity.account_id,
          'organization_id': identity.organization_id,
        }
    error = claude_auth.ClaudeError.parse({
      'schema_version': value.schema_version,
      'kind': value.kind,
      'retry': value.retry_disposition,
      'recovery': value.recovery,
      'details': details,
    })
    profile = claude_auth.compatibility_profile(value.compatibility_profile)
    return claude_commands.error_text(error, profile)
  except (ValueError, OverflowError, claude_auth.ProtocolError):
    return (
      'Claude subscription returned an unsupported error contract. '
      'Upgrade Mirror and retry; the request was not replayed.'
    )
