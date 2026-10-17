"""Resolve the same native executable for sessions and private commands."""

from __future__ import annotations

import os
import pathlib


def command(package_root: pathlib.Path | None = None) -> list[str]:
  """Return a worker argv, preserving executable paths containing spaces."""
  worker = os.environ.get('MIRROR_RUST_WORKER')
  if worker is not None:
    return [worker]
  bundled = (package_root or pathlib.Path(__file__).parent) / 'bin/mirror-agent'
  if bundled.is_file() and os.access(bundled, os.X_OK):
    return [str(bundled)]
  raise SystemExit(
    'mirror: no executable agent bundled; install a platform wheel or '
    'set MIRROR_RUST_WORKER to a built mirror-agent executable'
  )
