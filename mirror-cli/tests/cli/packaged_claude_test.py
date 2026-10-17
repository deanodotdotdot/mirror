"""Hermetic checks of an installed wheel's real Claude native authority.

build.sh runs these beside the existing OpenAI PTY and bundled-tool checks.
The Python-only ci.sh wheel has no native worker and must skip this module.
Login cancellation uses only a local listener and a rejected synthetic callback;
no token exchange, real credentials or provider HTTP requests are used.
"""

from __future__ import annotations

import contextlib
import datetime
import json
import os
import pathlib
import pty
import re
import select
import shutil
import signal
import socket
import stat
import subprocess
import sys
import termios
import time
import urllib.parse
import uuid

import pytest

from mirror import config

pytestmark = pytest.mark.skipif(
  os.environ.get('MIRROR_TEST_PACKAGED_WHEEL') != '1',
  reason='Requires an installed native wheel',
)

_PROFILE = 'mirror-phase0-omp-df4f996-v1'
_API_CANARY = 'SYNTHETIC_PACKAGED_CLAUDE_API_CANARY'
_BEARER_CANARY = 'SYNTHETIC_PACKAGED_CLAUDE_BEARER_CANARY'
_INPUT_CANARY = 'SYNTHETIC_PACKAGED_PRIVATE_INPUT_CANARY'
_ACCESS_CANARY = 'sk-ant-oat-SYNTHETIC_PACKAGED_ACCESS_CANARY'
_REFRESH_CANARY = 'SYNTHETIC_PACKAGED_REFRESH_CANARY'


@pytest.fixture(params=['bundled', 'helper path with spaces'])
def installed(
  tmp_path: pathlib.Path, request: pytest.FixtureRequest
) -> tuple[pathlib.Path, pathlib.Path, dict[str, str]]:
  cli = pathlib.Path(sys.executable).parent / 'mirror'
  native = pathlib.Path(config.__file__).parent / 'bin/mirror-agent'
  assert cli.is_file() and os.access(native, os.X_OK)
  # A ci.sh shell fixture is not native acceptance, even if the flag is set.
  with native.open('rb') as binary:
    assert binary.read(4) in (b'\x7fELF', b'\xcf\xfa\xed\xfe')
  home = tmp_path / 'isolated home'
  home.mkdir(mode=0o700)
  env = {
    'PATH': os.environ.get('PATH', os.defpath),
    'HOME': str(home),
    'TERM': 'xterm-256color',
    'LANG': 'C.UTF-8',
    'MIRROR_EXTENSIONS_ENABLED': 'false',
    'MIRROR_SKIP_UPDATE': '1',
    'MIRROR_MODELS_CONFIG': str(home / 'models.json'),
    'MIRROR_STATE_DB': str(home / 'state.db'),
    'MIRROR_CLI_DB': str(home / 'cli.db'),
    'ANTHROPIC_API_KEY': _API_CANARY,
    'OPENAI_API_KEY': _API_CANARY,
    'OPENROUTER_API_KEY': _API_CANARY,
    'CLAUDE_CODE_OAUTH_TOKEN': _BEARER_CANARY,
    'ANTHROPIC_OAUTH_TOKEN': _BEARER_CANARY,
  }
  if library_path := os.environ.get('LD_LIBRARY_PATH'):
    env['LD_LIBRARY_PATH'] = library_path
  if request.param != 'bundled':
    target = tmp_path / str(request.param) / 'real mirror-agent'
    target.parent.mkdir()
    shutil.copy2(native, target)
    native = target
    env['MIRROR_RUST_WORKER'] = str(native)
  return cli, native, env


def _clean_output(output: str) -> None:
  for forbidden in (
    _API_CANARY,
    _BEARER_CANARY,
    _INPUT_CANARY,
    _ACCESS_CANARY,
    _REFRESH_CANARY,
    'access_token',
    'refresh_token',
    'code_verifier',
    'authorization_url',
    'Traceback',
  ):
    if forbidden in output:
      pytest.fail('Installed command exposed a private field or canary')


def _no_chat_files(env: dict[str, str], *, logged_out: bool = False) -> None:
  home = pathlib.Path(env['HOME'])
  expected = (
    {'.mirror', '.mirror/claude_oauth.json', '.mirror/claude_oauth.lock'}
    if logged_out
    else set()
  )
  assert {str(path.relative_to(home)) for path in home.rglob('*')} == expected
  for path in home.rglob('*'):
    assert stat.S_IMODE(path.stat().st_mode) == (
      0o700 if path.is_dir() else 0o600
    )
    if path.is_file():
      _clean_output(path.read_text())


def _native(
  installed: tuple[pathlib.Path, pathlib.Path, dict[str, str]],
  action: str,
  *,
  params: dict[str, object] | None = None,
  code: int = 0,
) -> dict[str, object]:
  _, worker, env = installed
  request_id = str(uuid.uuid4())
  request = {
    'schema_version': 1,
    'request_id': request_id,
    'type': 'request',
    'provider': 'anthropic-oauth',
    'action': action,
    'params': {} if params is None else params,
  }
  with subprocess.Popen(
    [worker, 'auth', '--stdio'],
    cwd=env['HOME'],
    env=env,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
  ) as child:
    assert child.stdin is not None
    child.stdin.write(json.dumps(request) + '\n')
    child.stdin.flush()
    try:
      # Closing stdin before the terminal result cancels the private protocol.
      # These local operations return one small event, well below pipe capacity.
      assert child.wait(timeout=15) == code
    except BaseException:
      child.kill()
      child.wait(timeout=5)
      raise
    stdout, stderr = child.communicate(timeout=5)
  assert stderr == ''
  _clean_output(stdout)
  lines = stdout.splitlines()
  assert len(lines) == 1, 'Exactly one terminal private event is required'
  event = json.loads(lines[0])
  assert event['schema_version'] == 1
  assert event['request_id'] == request_id
  assert event['provider'] == 'anthropic-oauth'
  assert event['compatibility_profile'] == _PROFILE
  assert event['action'] == action
  assert event['type'] == ('completed' if code == 0 else 'error')
  return event


def _cli(
  installed: tuple[pathlib.Path, pathlib.Path, dict[str, str]],
  *args: str,
  code: int = 0,
) -> subprocess.CompletedProcess[str]:
  cli, _, env = installed
  completed = subprocess.run(
    [cli, *args],
    cwd=env['HOME'],
    env=env,
    stdin=subprocess.DEVNULL,
    capture_output=True,
    text=True,
    timeout=20,
    check=False,
  )
  assert completed.returncode == code
  _clean_output(completed.stdout + completed.stderr)
  return completed


def test_installed_native_local_status_logout_and_missing_usage(installed):
  """Real binary, fresh processes, isolated HOME and conflicting API canaries."""
  _, _, env = installed
  result = _native(installed, 'status')
  assert result['result']['status']['state'] == 'logged_out'
  assert result['result']['status']['source'] == 'Mirror login'
  result = _native(installed, 'usage', code=1)
  assert result['error']['kind'] == 'login_required'
  assert result['error']['recovery'] == 'login'
  _no_chat_files(env)
  result = _native(installed, 'logout')
  assert result['result']['status']['state'] == 'logged_out'
  result = _native(installed, 'status')
  assert result['result']['status']['state'] == 'logged_out'
  _no_chat_files(env, logged_out=True)
  record = json.loads(
    (pathlib.Path(env['HOME']) / '.mirror/claude_oauth.json').read_text()
  )
  assert record['credential'] is None and record['revision'] == 0


def test_installed_native_rejects_private_secret_params(installed):
  result = _native(
    installed, 'usage', params={'access_token': _INPUT_CANARY}, code=1
  )
  assert result['error']['kind'] == 'invalid_configuration'
  assert result['phase'] == 'protocol'
  _no_chat_files(installed[2])


def test_installed_cli_auth_status_logout_and_missing_usage(installed):
  """Shipping prechat commands must not create chat/trust/log/model state."""
  _, _, env = installed
  completed = _cli(installed, 'auth', 'status', 'claude', '--json')
  status = json.loads(completed.stdout)
  assert status['state'] == 'logged_out'
  assert status['source'] == 'Mirror login'
  assert status['compatibility_profile'] == _PROFILE
  assert 'request_id' not in status and 'result' not in status
  human = _cli(installed, 'auth', 'status', 'claude')
  assert 'Mirror login' in human.stdout + human.stderr
  for args in [('usage', 'claude'), ('usage', 'claude', '--json')]:
    missing = _cli(installed, *args, code=1)
    # Human recovery text or the closed JSON recovery enum; never zero usage.
    output = missing.stdout + missing.stderr
    assert 'login' in output.lower()
    assert 'used_percent' not in output
  _no_chat_files(env)
  _cli(installed, 'auth', 'logout', 'claude')
  result = _native(installed, 'status')
  assert result['result']['status']['state'] == 'logged_out'
  _no_chat_files(env, logged_out=True)


def test_installed_native_preserves_omp_notices():
  binary_dir = pathlib.Path(config.__file__).parent / 'bin'
  notice = (binary_dir / 'NOTICE').read_text()
  assert 'oh-my-pi' in notice
  assert 'df4f996e66a200ab7e0496114d43bdf85f32194d' in notice
  assert 'Copyright (c) 2025-2026 Can Bölük' in notice
  assert 'Permission is hereby granted, free of charge' in notice
  for name in ('LICENSE', 'RIPGREP-LICENSE-MIT', 'THIRD_PARTY_NOTICES.txt'):
    assert (binary_dir / name).read_text().strip()


def test_installed_cli_status_never_copies_seeded_credentials(installed):
  _, _, env = installed
  home = pathlib.Path(env['HOME'])
  directory = home / '.mirror'
  directory.mkdir(mode=0o700)
  now = datetime.datetime.now(datetime.UTC)
  identity = {
    'account_id': '11111111-1111-4111-8111-111111111111',
    'organization_id': '22222222-2222-4222-8222-222222222222',
  }
  record = {
    'schema_version': 1,
    'generation': str(uuid.uuid4()),
    'revision': 1,
    'installation_id': str(uuid.uuid4()),
    'credential': identity
    | {
      'access_token': _ACCESS_CANARY,
      'refresh_token': _REFRESH_CANARY,
      'authorized_at': (now - datetime.timedelta(hours=1)).isoformat(),
      'expires_at': (now + datetime.timedelta(hours=1)).isoformat(),
      'email': None,
      'organization_name': None,
      'scopes': ['user:inference'],
    },
  }
  credential = directory / 'claude_oauth.json'
  credential.write_text(json.dumps(record))
  credential.chmod(0o600)
  lock = directory / 'claude_oauth.lock'
  lock.touch(mode=0o600)
  before = credential.read_bytes()
  status = json.loads(
    _cli(installed, 'auth', 'status', 'claude', '--json').stdout
  )
  assert status['state'] == 'usable' and status['identity'] == identity
  _cli(installed, 'auth', 'status', 'claude')
  assert credential.read_bytes() == before
  # Only the deliberately seeded private grant may contain these canaries.
  assert set(home.rglob('*')) == {directory, credential, lock}
  for path in home.rglob('*'):
    if path.is_file() and path != credential:
      _clean_output(path.read_text())
  _cli(installed, 'auth', 'logout', 'claude')
  _no_chat_files(env, logged_out=True)


def test_installed_cli_private_callback_cancel_restores_terminal(installed):
  cli, _, env = installed
  master, slave = pty.openpty()
  before = termios.tcgetattr(slave)
  child = None
  native_pids: list[int] = []
  private = bytearray()

  def await_notice(notice: bytes) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
      if notice in private:
        return
      if child is not None and child.poll() is not None:
        break
      if select.select([master], [], [], 0.05)[0]:
        try:
          private.extend(os.read(master, 65536))
        except OSError:
          break
        if len(private) > 131072:
          pytest.fail('Private terminal output exceeded the fixture bound')
    # Never print private output or a generated authorization URL on failure.
    pytest.fail('Installed login did not reach the expected terminal phase')

  try:
    child = subprocess.Popen(
      [
        sys.executable,
        '-c',
        'import os,sys; os.login_tty(0); os.execv(sys.argv[1], sys.argv[1:])',
        str(cli),
        'auth',
        'login',
        'claude',
        '--no-browser',
      ],
      stdin=slave,
      stdout=slave,
      stderr=slave,
      cwd=env['HOME'],
      env=env,
    )
    await_notice(b'Ctrl-C cancels.')
    assert not termios.tcgetattr(slave)[3] & termios.ECHO
    # Inspect only process IDs, never command lines or environment contents.
    processes = subprocess.run(
      ['ps', '-axo', 'pid=,ppid='],
      capture_output=True,
      text=True,
      timeout=5,
      check=True,
    )
    native_pids = [
      int(pid)
      for line in processes.stdout.splitlines()
      for pid, parent in [line.split()]
      if int(parent) == child.pid
    ]
    assert len(native_pids) == 1, (
      'Expected one private native authority process'
    )
    match = re.search(rb'https://claude.ai/oauth/authorize\?[^\s]+', private)
    if match is None:
      pytest.fail('Installed login did not provide its local callback setup')
    try:
      query = urllib.parse.parse_qs(
        urllib.parse.urlsplit(match[0].decode()).query
      )
      redirect = urllib.parse.urlsplit(query['redirect_uri'][0])
    except (UnicodeError, ValueError, KeyError, IndexError):
      pytest.fail('Installed login provided an invalid callback setup')
    if redirect.hostname != 'localhost' or redirect.port is None:
      pytest.fail('Unexpected callback listener address')
    port = redirect.port
    # Wrong state is rejected before exchange; no real code is ever submitted.
    os.write(master, (_INPUT_CANARY + '#wrong-state\n').encode())
    await_notice(b'Callback rejected;')
    if _INPUT_CANARY.encode() in private:
      pytest.fail('Private callback input was echoed')
    os.write(master, b'\x03')
    assert child.wait(timeout=10) == 130
    assert termios.tcgetattr(slave) == before
    for pid in native_pids:
      with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)
    # The foreground command is reaped; the native listener must also be closed.
    for family, host in (
      (socket.AF_INET, '127.0.0.1'),
      (socket.AF_INET6, '::1'),
    ):
      with socket.socket(family, socket.SOCK_STREAM) as connection:
        connection.settimeout(1)
        assert connection.connect_ex((host, port)) != 0
    _no_chat_files(env)
  finally:
    if child is not None and child.poll() is None:
      with contextlib.suppress(ProcessLookupError):
        os.killpg(child.pid, signal.SIGKILL)
      child.wait(timeout=5)
    for pid in native_pids:
      with contextlib.suppress(ProcessLookupError):
        os.kill(pid, signal.SIGKILL)
    os.close(master)
    os.close(slave)
