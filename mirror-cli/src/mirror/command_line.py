"""Lightweight recognition of commands which must not start a conversation."""

from __future__ import annotations

from collections.abc import Sequence


def private_command(arguments: Sequence[str]) -> bool:
  """Recognize the reserved command position, never a quoted exec prompt."""
  arguments = tuple(arg for arg in arguments if arg != '--no-update')
  return bool(arguments and arguments[0] in ('auth', 'usage'))
