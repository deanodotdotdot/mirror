"""Private command framing and actual controlling-terminal login behavior."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import pathlib
import pty
import select
import signal
import subprocess
import sys
import termios
import time
from collections.abc import Generator
from unittest import mock

import pytest

from mirror import claude_commands, command_line
from mirror.cli import claude_auth, main
from mirror.cli import logging as mirror_logging

_CANARY = 'SYNTHETIC_PRIVATE_CALLBACK_DO_NOT_LOG'
_STATUS = {
  'schema_version': 1,
  'provider': 'anthropic-oauth',
  'source': 'Mirror login',
  'state': 'usable',
  'identity': {
    'account_id': '11111111-1111-4111-8111-111111111111',
    'organization_id': '22222222-2222-4222-8222-222222222222',
  },
  'display': {
    'email': 'test@example.invalid',
    'organization_name': 'Test workspace',
  },
  'expires_at': '2026-10-17T19:00:00Z',
  'recovery': 'none',
  'error': None,
}
_ERROR: dict[str, object] = {
  'schema_version': 1,
  'kind': 'login_required',
  'retry': 'terminal',
  'recovery': 'login',
  'details': {},
}
_PRELUDE = f"""
import json, os, select, signal, sys, time
assert sys.argv[1:] == ['auth', '--stdio']
request = json.loads(sys.stdin.readline())
assert request['type'] == 'request' and request['params'] == {{}}
base = {{key: request[key] for key in ('schema_version', 'request_id', 'provider', 'action')}}
base['compatibility_profile'] = 'synthetic-fixture-v1'
def emit(kind, **fields):
  print(json.dumps(base | {{'type': kind}} | fields), flush=True)
STATUS = {_STATUS!r}
ERROR = {_ERROR!r}
"""


@pytest.fixture(autouse=True)
def isolated_home(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.setenv('HOME', str(tmp_path))


def _worker(tmp_path: pathlib.Path, body: str) -> pathlib.Path:
  path = tmp_path / 'worker directory with spaces' / 'fake native worker'
  path.parent.mkdir(exist_ok=True)
  path.write_text(f'#!{sys.executable}\n' + _PRELUDE + body)
  path.chmod(0o755)
  return path


@pytest.mark.parametrize(
  'arguments',
  [
    ['auth'],
    ['auth', 'status', 'claude'],
    ['--no-update', 'usage', 'claude'],
  ],
)
def test_reserved_commands(arguments: list[str]) -> None:
  assert command_line.private_command(arguments)


@pytest.mark.parametrize(
  'arguments',
  [
    ['exec', 'auth'],
    ['exec', 'auth login claude'],
    ['auth login claude'],
  ],
)
def test_exec_prompts_are_not_reserved(arguments: list[str]) -> None:
  assert not command_line.private_command(arguments)


@pytest.mark.parametrize('entry', ['main', 'run'])
def test_private_commands_precede_logging_trust_and_chat(
  entry: str,
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.setattr(sys, 'argv', ['mirror', 'auth', 'status', 'claude'])
  with (
    mock.patch.object(claude_commands, 'run', return_value=0) as run,
    mock.patch.object(
      mirror_logging,
      'configure_logging',
      side_effect=AssertionError('normal logs started'),
    ),
    mock.patch.object(
      main, '_parse_args', side_effect=AssertionError('chat parsed')
    ),
    mock.patch.object(
      main.onboarding,
      'ensure_consent',
      side_effect=AssertionError('trust requested'),
    ),
    pytest.raises(SystemExit) as exited,
  ):
    getattr(main, entry)()
  assert exited.value.code == 0
  run.assert_called_once_with(['auth', 'status', 'claude'])


@pytest.mark.parametrize(
  'arguments',
  [
    ['auth'],
    ['auth', 'status'],
    ['auth', 'login', 'claude', '--json'],
    ['usage', 'other'],
    ['auth', 'login', 'claude', _CANARY],
  ],
)
def test_reserved_parser_errors_do_not_echo_input(
  arguments: list[str],
  capsys: pytest.CaptureFixture[str],
) -> None:
  with pytest.raises(SystemExit) as error:
    claude_commands.parse_args(arguments)
  assert error.value.code == 2
  assert _CANARY not in capsys.readouterr().err


@pytest.mark.asyncio
async def test_status_uses_private_pipes_keeps_stdin_open_and_discards_stderr(
  tmp_path: pathlib.Path,
  capfd: pytest.CaptureFixture[str],
) -> None:
  executable = _worker(
    tmp_path,
    f"""
assert not select.select([sys.stdin], [], [], 0.05)[0], 'stdin closed early'
print({_CANARY!r}, file=sys.stderr, flush=True)
emit('completed', result={{'status': STATUS}})
sys.exit(1)  # A delivered completed result wins a later exit-status race.
""",
  )
  async with claude_auth.NativeCommand([str(executable)], 'status') as command:
    event = await command.read_event()
    assert isinstance(event, claude_auth.Completed)
    assert event.status.to_json() == _STATUS
  captured = capfd.readouterr()
  assert captured.out + captured.err == ''
  assert not (tmp_path / '.mirror').exists()


@pytest.mark.asyncio
async def test_exact_native_output_limit_excludes_newline(
  tmp_path: pathlib.Path,
) -> None:
  executable = _worker(
    tmp_path,
    """
payload = json.dumps(base | {'type': 'completed', 'result': {'status': STATUS}})
print(payload + ' ' * (4 * 1024 * 1024 - len(payload.encode())), flush=True)
""",
  )
  async with claude_auth.NativeCommand([str(executable)], 'status') as command:
    assert isinstance(await command.read_event(), claude_auth.Completed)


@pytest.mark.asyncio
async def test_invalid_callback_unicode_has_no_payload_in_exception(
  tmp_path: pathlib.Path,
) -> None:
  executable = _worker(
    tmp_path,
    """
emit('progress', phase='awaiting_callback', notice=None)
request = json.loads(sys.stdin.readline())
assert request['type'] == 'cancel'
""",
  )
  with pytest.raises(claude_auth.ProtocolError) as error:
    async with claude_auth.NativeCommand([str(executable)], 'login') as command:
      await command.read_event()
      await command.callback(_CANARY + '\ud800')
  assert _CANARY not in str(error.value)


@pytest.mark.asyncio
async def test_callback_and_cancel_only_travel_over_private_stdin(
  tmp_path: pathlib.Path,
) -> None:
  marker = tmp_path / 'cancelled'
  executable = _worker(
    tmp_path,
    f"""
emit('progress', phase='awaiting_callback', notice=None)
callback = json.loads(sys.stdin.readline())
assert callback == {{'schema_version': 1, 'request_id': request['request_id'], 'type': 'callback', 'value': {_CANARY!r}}}
emit('progress', phase='exchanging_code', notice=None)
cancel = json.loads(sys.stdin.readline())
assert cancel == {{'schema_version': 1, 'request_id': request['request_id'], 'type': 'cancel'}}
with open({str(marker)!r}, 'w') as output:
  output.write('cancel received')
""",
  )
  async with claude_auth.NativeCommand([str(executable)], 'login') as command:
    assert isinstance(await command.read_event(), claude_auth.Progress)
    await command.callback(_CANARY)
    assert isinstance(await command.read_event(), claude_auth.Progress)
    process = command._process  # pyright: ignore[reportPrivateUsage]
  assert marker.read_text() == 'cancel received'
  assert process is not None and process.returncode == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
  'body',
  [
    f'print({_CANARY!r}, flush=True)',
    "base['request_id'] = 'wrong'; emit('completed', result={'status': STATUS})",
    "base.pop('compatibility_profile'); emit('completed', result={'status': STATUS})",
    "STATUS['access_token'] = '"
    + _CANARY
    + "'; emit('completed', result={'status': STATUS})",
    "ERROR['raw_body'] = '"
    + _CANARY
    + "'; emit('error', phase='preparing', error=ERROR, commit_outcome=None)",
    "emit('authorization_url', authorization_url='https://attacker.invalid/oauth/authorize', expires_at='2026-10-17T19:00:00Z')",
    "print('x' * (4 * 1024 * 1024 + 1), flush=True)",
  ],
  ids=[
    'malformed_json',
    'wrong_request_id',
    'missing_profile',
    'unexpected_status_secret',
    'unexpected_error_body',
    'untrusted_authorization_host',
    'oversized_frame',
  ],
)
async def test_invalid_native_output_is_bounded_and_payload_free(
  tmp_path: pathlib.Path,
  body: str,
  capfd: pytest.CaptureFixture[str],
) -> None:
  executable = _worker(tmp_path, body)
  with pytest.raises(claude_auth.ProtocolError) as error:
    async with claude_auth.NativeCommand([str(executable)], 'login') as command:
      await command.read_event()
  assert _CANARY not in str(error.value)
  assert _CANARY not in ''.join(capfd.readouterr())


@pytest.mark.asyncio
async def test_eof_without_terminal_result_has_unknown_commit_outcome(
  tmp_path: pathlib.Path,
) -> None:
  executable = _worker(tmp_path, 'sys.exit(0)\n')
  with pytest.raises(
    claude_auth.CommandInterrupted, match='check the saved login'
  ):
    async with claude_auth.NativeCommand([str(executable)], 'login') as command:
      await command.read_event()


@pytest.mark.asyncio
async def test_timeout_cancels_native_without_closing_stdin_early(
  tmp_path: pathlib.Path,
) -> None:
  marker = tmp_path / 'timed out'
  executable = _worker(
    tmp_path,
    f"""
emit('progress', phase='awaiting_callback', notice=None)
request = json.loads(sys.stdin.readline())
assert request['type'] == 'cancel'
with open({str(marker)!r}, 'w') as output:
  output.write('cancel received')
""",
  )
  with pytest.raises(claude_auth.CommandInterrupted):
    async with claude_auth.NativeCommand([str(executable)], 'login') as command:
      await command.read_event()
      command._deadline = time.monotonic()  # pyright: ignore[reportPrivateUsage]
      await command.read_event()
  assert marker.read_text() == 'cancel received'


@pytest.mark.asyncio
async def test_received_completed_result_wins_cancellation_during_reaping(
  tmp_path: pathlib.Path,
) -> None:
  executable = _worker(
    tmp_path,
    """
emit('completed', result={'status': STATUS})
time.sleep(0.25)
""",
  )
  delivered = asyncio.Event()

  async def operation() -> claude_auth.Event:
    async with claude_auth.NativeCommand(
      [str(executable)], 'status'
    ) as command:
      event = await command.read_event()
      delivered.set()
      return event

  task = asyncio.create_task(operation())
  await delivered.wait()
  task.cancel()
  assert isinstance(await task, claude_auth.Completed)


def test_json_status_creates_no_chat_state_and_preserves_model_selection(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  capsys: pytest.CaptureFixture[str],
) -> None:
  executable = _worker(
    tmp_path, "emit('completed', result={'status': STATUS})\n"
  )
  monkeypatch.setenv('MIRROR_RUST_WORKER', str(executable))
  selected = pathlib.Path(os.environ['MIRROR_MODELS_CONFIG']).with_name(
    'last_model.json'
  )
  selected.write_text('previous selection')
  assert claude_commands.run(['auth', 'status', 'claude', '--json']) == 0
  result = json.loads(capsys.readouterr().out)
  assert result == _STATUS | {'compatibility_profile': 'synthetic-fixture-v1'}
  assert selected.read_text() == 'previous selection'
  assert not (tmp_path / '.mirror').exists()


def test_login_without_controlling_terminal_is_actionable_and_payload_free(
  tmp_path: pathlib.Path,
) -> None:
  executable = _worker(
    tmp_path,
    """
request = json.loads(sys.stdin.readline())
assert request['type'] == 'cancel'
""",
  )
  result = subprocess.run(
    [
      sys.executable,
      '-m',
      'mirror.claude_commands',
      'auth',
      'login',
      'claude',
      '--no-browser',
    ],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    start_new_session=True,
    timeout=10,
    env=os.environ | {'MIRROR_RUST_WORKER': str(executable)},
  )
  assert result.returncode == 1
  assert 'private controlling terminal' in result.stderr
  assert 'mirror auth login claude --no-browser' in result.stderr
  assert 'https://' not in result.stdout + result.stderr


def test_native_compatibility_profile_reaches_safe_json_and_human_error(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  capsys: pytest.CaptureFixture[str],
) -> None:
  executable = _worker(
    tmp_path,
    """
ERROR.update(kind='compatibility_error', recovery='upgrade')
emit('error', phase='preparing', error=ERROR, commit_outcome='not_committed')
""",
  )
  monkeypatch.setenv('MIRROR_RUST_WORKER', str(executable))
  assert claude_commands.run(['auth', 'status', 'claude', '--json']) == 1
  result = json.loads(capsys.readouterr().out)
  assert result['compatibility_profile'] == 'synthetic-fixture-v1'
  assert result['error']['kind'] == 'compatibility_error'
  assert claude_commands.run(['auth', 'status', 'claude']) == 1
  assert (
    'Compatibility profile: synthetic-fixture-v1' in capsys.readouterr().err
  )


def test_additive_error_enums_are_safe_and_rate_limit_is_not_quota_exhaustion() -> (
  None
):
  error = claude_auth.ClaudeError.parse(
    _ERROR
    | {
      'kind': 'rate_limited',
      'recovery': 'retry_later',
      'details': {'retry_at': '2026-10-17T19:00:00Z'},
    }
  )
  text = claude_commands.error_text(error)
  assert 'rate limit' in text
  assert 'expired' not in text and 'usage limit' not in text
  future = claude_auth.ClaudeError.parse(
    _ERROR | {'kind': _CANARY, 'details': {'configuration': _CANARY}}
  )
  assert _CANARY not in str(future.to_json()) + claude_commands.error_text(
    future
  )
  compatibility = claude_auth.ClaudeError.parse(
    _ERROR | {'kind': 'compatibility_error'}
  )
  assert 'synthetic-fixture-v1' in claude_commands.error_text(
    compatibility, 'synthetic-fixture-v1'
  )


def test_json_status_preserves_nulls_and_utc_while_human_time_is_local(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  previous = os.environ.get('TZ')
  try:
    monkeypatch.setenv('TZ', 'America/New_York')
    time.tzset()
    status = claude_auth.AuthStatus.parse(_STATUS)
    assert status.to_json()['expires_at'] == '2026-10-17T19:00:00Z'
    assert '15:00:00 EDT (UTC-0400)' in claude_commands.status_text(status)
    empty = claude_auth.AuthStatus.parse(
      _STATUS
      | {
        'state': 'logged_out',
        'identity': None,
        'display': None,
        'expires_at': None,
        'recovery': 'login',
      }
    )
    assert empty.to_json()['identity'] is None
    assert 'unavailable' in claude_commands.status_text(empty)
    assert 'mirror auth login claude' in claude_commands.status_text(empty)
  finally:
    if previous is None:
      os.environ.pop('TZ', None)
    else:
      os.environ['TZ'] = previous
    time.tzset()


def test_json_local_error_does_not_include_os_error_payload(
  monkeypatch: pytest.MonkeyPatch,
  capsys: pytest.CaptureFixture[str],
) -> None:
  async def fail(_: object) -> None:
    raise OSError(_CANARY)

  monkeypatch.setattr(claude_commands, '_execute', fail)
  assert claude_commands.run(['auth', 'status', 'claude', '--json']) == 1
  captured = capsys.readouterr()
  assert _CANARY not in captured.out + captured.err
  result = json.loads(captured.out)
  assert result['schema_version'] == 1
  assert result['commit_outcome'] == 'unknown'


@contextlib.contextmanager
def _controlling_login(
  tmp_path: pathlib.Path,
  mode: str,
) -> Generator[tuple[int, int, int]]:
  native_body = """
emit('authorization_url', authorization_url='https://claude.ai/oauth/authorize?state=SYNTHETIC_STATE', expires_at='2026-10-17T19:00:00Z')
"""
  if mode == 'automatic':
    native_body += (
      "time.sleep(0.2); emit('completed', result={'status': STATUS})\n"
    )
  elif mode == 'cancel_committed':
    native_body += (
      "assert json.loads(sys.stdin.readline())['type'] == 'cancel'\n"
      "emit('completed', result={'status': STATUS})\n"
    )
  elif mode == 'cancel':
    native_body += (
      'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
      'json.loads(sys.stdin.readline()); time.sleep(10)\n'
    )
  else:
    native_body += f"""
callback = json.loads(sys.stdin.readline())
assert callback['value'] == {_CANARY!r}
emit('completed', result={{'status': STATUS}})
"""
  executable = _worker(tmp_path, native_body)
  result_read, result_write = os.pipe()
  os.set_inheritable(result_write, True)
  script = tmp_path / 'controlling terminal child.py'
  script.write_text(f"""
import contextlib, io, json, os, termios
from mirror import claude_commands
from mirror.cli import claude_auth
async def no_browser(url):
  return False
claude_auth.open_browser = no_browser
tty = os.open('/dev/tty', os.O_RDWR | os.O_NOCTTY)
before = termios.tcgetattr(tty)
os.close(tty)
stdout, stderr = io.StringIO(), io.StringIO()
try:
  with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
    code = claude_commands.run(['auth', 'login', 'claude'])
  tty = os.open('/dev/tty', os.O_RDWR | os.O_NOCTTY)
  restored = termios.tcgetattr(tty) == before
  os.close(tty)
  public = stdout.getvalue() + stderr.getvalue()
  result = {{'code': code, 'restored': restored, 'redacted': {_CANARY!r} not in public and 'https://' not in public}}
except BaseException as error:
  result = {{'harness_failure': type(error).__name__}}
os.write({result_write}, json.dumps(result).encode())
""")
  pid, master = pty.fork()
  if pid == 0:
    os.close(result_read)
    os.execve(
      sys.executable,
      [sys.executable, str(script)],
      os.environ
      | {
        'MIRROR_RUST_WORKER': str(executable),
        'GRPC_ENABLE_FORK_SUPPORT': '0',
      },
    )
  os.close(result_write)
  try:
    yield pid, master, result_read
  finally:
    with contextlib.suppress(ProcessLookupError):
      os.kill(pid, signal.SIGKILL)
    with contextlib.suppress(ChildProcessError):
      os.waitpid(pid, 0)
    os.close(master)
    os.close(result_read)


@pytest.mark.parametrize(
  'mode', ['manual', 'automatic', 'cancel', 'cancel_committed']
)
def test_actual_controlling_tty_input_completion_and_cancellation(
  tmp_path: pathlib.Path,
  mode: str,
) -> None:
  with _controlling_login(tmp_path, mode) as (pid, master, result_fd):
    private = bytearray()
    result = bytearray()
    sent = False
    cancelled_at: float | None = None
    restored_after: float | None = None
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
      ready, _, _ = select.select([master, result_fd], [], [], 0.05)
      for fd in ready:
        try:
          chunk = os.read(fd, 16384)
        except OSError:
          chunk = b''
        if fd == result_fd:
          result.extend(chunk)
        else:
          private.extend(chunk)
      if b'Ctrl-C cancels.' in private and not sent:
        if mode != 'automatic':
          os.write(
            master,
            b'\x03'
            if mode in ('cancel', 'cancel_committed')
            else _CANARY.encode() + b'\n',
          )
          if mode in ('cancel', 'cancel_committed'):
            cancelled_at = time.monotonic()
        sent = True
      if mode in ('cancel', 'cancel_committed') and sent:
        if (
          termios.tcgetattr(master)[3] & termios.ECHO and restored_after is None
        ):
          assert cancelled_at is not None
          restored_after = time.monotonic() - cancelled_at
      if os.waitpid(pid, os.WNOHANG)[0]:
        if not result:
          result.extend(os.read(result_fd, 16384))
        break
    assert result, 'controlling terminal child failed to finish'
    summary = json.loads(result)
    assert summary == {
      'code': 130 if mode == 'cancel' else 0,
      'restored': True,
      'redacted': True,
    }
    assert _CANARY.encode() not in private
    assert b'Leave this command running' in private
    assert b'localhost browser error is expected' in private
    assert b'press Enter' in private and b'Input is hidden' in private
    if mode == 'manual':
      assert b'Browser launch failed' in private
    if mode == 'cancel':
      assert restored_after is not None and restored_after < 1


@pytest.mark.asyncio
@pytest.mark.parametrize('committed', [False, True])
async def test_native_terminal_during_cancel_is_authoritative(
  tmp_path: pathlib.Path, committed: bool
) -> None:
  body = """
emit('progress', phase='committing', notice=None)
control = json.loads(sys.stdin.readline())
assert control['type'] == 'cancel'
"""
  body += (
    "emit('completed', result={'status': STATUS})\n"
    if committed
    else "emit('error', error=ERROR | {'kind': 'login_cancelled', 'recovery': 'none'}, phase='committing', commit_outcome='not_committed')\n"
  )
  executable = _worker(tmp_path, body)
  async with claude_auth.NativeCommand([str(executable)], 'login') as command:
    assert isinstance(await command.read_event(), claude_auth.Progress)
    result = await command.cancel_result()
  if committed:
    assert isinstance(result, claude_auth.Completed)
  else:
    assert isinstance(result, claude_auth.Failed)
    assert result.commit_outcome == 'not_committed'


@pytest.mark.asyncio
async def test_installed_native_status_logout_protocol(
  tmp_path: pathlib.Path,
) -> None:
  executable = os.environ.get('MIRROR_TEST_NATIVE_WORKER')
  if not executable:
    pytest.skip(
      'Set MIRROR_TEST_NATIVE_WORKER to verify the built native command'
    )
  for action in ('status', 'logout', 'status'):
    async with claude_auth.NativeCommand([executable], action) as command:
      result = await command.read_event()
      assert isinstance(result, claude_auth.Completed)
      assert result.status.state == 'logged_out'
      assert result.status.identity is None
      assert result.compatibility_profile.startswith('mirror-phase0-')
    if action == 'status' and not (tmp_path / '.mirror').exists():
      assert list(tmp_path.iterdir()) == []


@pytest.mark.asyncio
async def test_operation_timeout_allows_native_terminal_during_cleanup(
  tmp_path: pathlib.Path,
) -> None:
  executable = _worker(
    tmp_path,
    """
emit('progress', phase='committing', notice=None)
assert json.loads(sys.stdin.readline())['type'] == 'cancel'
emit('completed', result={'status': STATUS})
""",
  )
  async with claude_auth.NativeCommand([str(executable)], 'login') as command:
    assert isinstance(await command.read_event(), claude_auth.Progress)
    command._deadline = time.monotonic() - 1
    with pytest.raises(claude_auth.CommandInterrupted):
      await command.read_event()
    assert isinstance(await command.cancel_result(), claude_auth.Completed)
