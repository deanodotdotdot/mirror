"""Exercise launcher signals and host-owned completion evidence without a VM."""

from __future__ import annotations

import asyncio
import pathlib
import sys

import pytest

from mirror.cli import sandbox_shell

_WORKER = """
import json, pathlib, signal, sys
if sys.argv[2] == 'quarantine':
    pathlib.Path('quarantined').touch()
else:
    with pathlib.Path('executed').open('a') as log:
        log.write('exec\\n')
    def terminate(*_):
        pathlib.Path(sys.argv[5]).write_text(json.dumps({sys.argv[6]: 'stopped'}))
        raise SystemExit(0)
    signal.signal(signal.SIGTERM, terminate)
    print('ready', flush=True)
    signal.pause()
"""


@pytest.mark.asyncio
@pytest.mark.parametrize(
  ('outcome', 'error', 'quarantined'),
  [('Err', TimeoutError, False), ('Unknown', RuntimeError, True)],
)
async def test_open_timeout_quarantines_only_unknown_completion(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  outcome: str,
  error: type[Exception],
  quarantined: bool,
) -> None:
  # Worker setup writes msb configuration, even when the worker is a fixture.
  monkeypatch.setenv('HOME', str(tmp_path))
  library = tmp_path / 'unused-libkrunfw'
  library.touch()
  monkeypatch.setenv('MSB_LIBKRUNFW_PATH', str(library))
  backend = sandbox_shell.SandboxShell(
    tmp_path,
    worker_command=(sys.executable, '-c', _WORKER),
    configuration={'id': 'fixture'},
  )

  with pytest.raises(error):
    async with backend.open([outcome]) as process:
      assert await process.read_stdout() == b'ready\n'
      async with asyncio.timeout(0):
        await process.wait()

  assert (tmp_path / 'quarantined').exists() == quarantined
  assert (tmp_path / 'executed').read_text() == 'exec\n'
