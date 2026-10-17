"""The launcher hands off before any of the expensive CLI initialization."""

from __future__ import annotations

import pathlib
import subprocess
import sys
from unittest import mock

import pytest

from mirror import distribution, main


def test_launcher_import_does_not_import_cli() -> None:
  subprocess.run(
    [
      sys.executable,
      '-c',
      'import sys; from mirror import main; '
      'assert "mirror.cli.main" not in sys.modules; '
      'assert "mirror.cli.cli_store" not in sys.modules; '
      'assert "grpc" not in sys.modules',
    ],
    check=True,
  )


def test_private_commands_import_without_chat_extensions_or_rpc() -> None:
  subprocess.run(
    [
      sys.executable,
      '-c',
      'import sys; import mirror.claude_commands; '
      'assert "mirror.cli.cli" not in sys.modules; '
      'assert "mirror.cli.cli_store" not in sys.modules; '
      'assert "mirror.cli.extensions.inference_controls.extension" not in sys.modules; '
      'assert "grpc" not in sys.modules',
    ],
    check=True,
  )


def test_handoff_preserves_arguments_and_cwd(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.chdir(tmp_path)
  monkeypatch.setenv('GRPC_ENABLE_FORK_SUPPORT', '1')
  monkeypatch.setattr(
    sys, 'argv', ['mirror', '--model', 'fixture', '--continue']
  )
  python = tmp_path / 'selected' / 'bin' / 'python'
  with (
    mock.patch.object(
      main.updater, 'prepare', return_value=main.updater.Launch(python)
    ) as prepare,
    mock.patch.object(main.os, 'execve') as execute,
    mock.patch.object(sys.stdin, 'isatty', return_value=True),
    mock.patch.object(sys.stderr, 'isatty', return_value=True),
  ):
    main.run()
  prepare.assert_called_once_with(
    check=True, interactive=True, update_only=False
  )
  execute.assert_called_once_with(
    str(python),
    [str(python), '-m', 'mirror.cli.main', '--model', 'fixture', '--continue'],
    mock.ANY,
  )
  assert pathlib.Path.cwd() == tmp_path
  assert execute.call_args.args[2]['GRPC_ENABLE_FORK_SUPPORT'] == '0'


@pytest.mark.parametrize(
  'option',
  [
    '--help',
    '-h',
    '--version',
    '--showcase',
    '--no-update',
    'exec',
    'auth',
    'usage',
  ],
)
def test_fast_paths_do_not_check(
  option: str, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.setattr(sys, 'argv', ['mirror', option])
  with (
    mock.patch.object(
      main.updater,
      'prepare',
      return_value=main.updater.Launch(pathlib.Path(sys.executable)),
    ) as prepare,
    mock.patch.object(main.os, 'execve'),
  ):
    main.run()
  assert prepare.call_args.kwargs['check'] is False


@pytest.mark.parametrize('command', ['auth', 'usage'])
def test_private_commands_handoff_without_tui_or_update_prompt(
  command: str,
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.setattr(sys, 'argv', ['mirror', command, 'status', 'claude'])
  python = pathlib.Path(sys.executable)
  with (
    mock.patch.object(
      main.updater, 'prepare', return_value=main.updater.Launch(python)
    ) as prepare,
    mock.patch.object(main.os, 'execve') as execute,
  ):
    main.run()
  prepare.assert_called_once_with(
    check=False, interactive=False, update_only=False
  )
  assert execute.call_args.args[1][1:3] == ['-m', 'mirror.claude_commands']


def test_console_version_contract_survives_handoff() -> None:
  result = subprocess.run(
    [sys.executable, '-m', 'mirror.main', '--version'],
    check=True,
    capture_output=True,
    text=True,
  )
  assert result.stdout.strip() == f'mirror {distribution.installed().version}'


def test_cached_release_owns_next_check_and_receives_original_flags(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.setattr(sys, 'argv', ['mirror', '--no-update', '--continue'])
  selected = pathlib.Path('/fixture/selected/bin/python')
  with (
    mock.patch.object(
      main.updater,
      'prepare',
      return_value=main.updater.Launch(selected, 'mirror.main'),
    ),
    mock.patch.object(main.os, 'execve') as execute,
  ):
    main.run()
  execute.assert_called_once_with(
    str(selected),
    [str(selected), '-m', 'mirror.main', '--no-update', '--continue'],
    mock.ANY,
  )
