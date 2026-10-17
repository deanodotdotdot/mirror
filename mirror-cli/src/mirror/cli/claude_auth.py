"""Private native Claude commands. No credential access or ordinary RPC logs."""

from __future__ import annotations

import asyncio
import dataclasses
import datetime
import json
import os
import re
import select
import sys
import termios
import time
import urllib.parse
import uuid
from collections.abc import Mapping, Sequence
from typing import TYPE_CHECKING, Protocol, cast

if TYPE_CHECKING:
  from mirror.cli.claude_metadata import Metadata

PROVIDER = 'anthropic-oauth'
SCHEMA_VERSION = 1
MAX_OUTPUT_LINE = 4 * 1024 * 1024
MAX_CALLBACK = 8192
_MAX_INPUT_LINE = 16 * 1024
_PHASES = frozenset((
  'protocol',
  'preparing',
  'awaiting_callback',
  'exchanging_code',
  'resolving_identity',
  'committing',
))
_KINDS = frozenset((
  'login_required',
  'login_expired',
  'login_cancelled',
  'login_timeout',
  'credential_storage_error',
  'account_changed',
  'subscription_limited',
  'model_unavailable',
  'access_denied',
  'auth_temporarily_unavailable',
  'compatibility_error',
  'invalid_configuration',
  'partial_stream_error',
  'request_cancelled',
  'rate_limited',
))
_RECOVERIES = frozenset((
  'none',
  'login',
  'reconnect',
  'retry_later',
  'repair_storage',
  'check_configuration',
  'upgrade',
))
_PROVIDER_CODES = frozenset((
  'authentication_error',
  'invalid_grant',
  'permission_error',
  'not_found_error',
  'rate_limit_error',
  'overloaded_error',
  'invalid_request_error',
  'api_error',
  'unknown',
))
_CONFIGURATIONS = frozenset((
  'api_key_not_allowed',
  'endpoint_not_allowed',
  'codex_auth_path_not_allowed',
  'missing_binding',
  'unknown_schema',
  'unknown_profile',
  'invalid_identity',
  'invalid_access_token',
  'unsupported_surface',
  'not_implemented',
  'invalid_request_options',
))


class ProtocolError(RuntimeError):
  """A fixed safe error, never carrying native output or parser fragments."""

  def __init__(self) -> None:
    super().__init__(
      'Claude command returned an invalid private response; upgrade Mirror and retry.'
    )


class CommandInterrupted(RuntimeError):
  """The terminal outcome was not delivered; a mutation may have committed."""

  def __init__(self) -> None:
    super().__init__(
      'Claude command ended without a result. Run mirror auth status claude '
      'to check the saved login before retrying.'
    )


class TerminalError(RuntimeError):
  """Only reviewed UI text may cross into a user-facing exception."""

  def __init__(self, *, cleanup: bool = False) -> None:
    super().__init__(
      'Could not restore the login terminal. Run stty echo in that terminal, '
      'then mirror auth status claude to check the login.'
      if cleanup
      else 'Claude login requires a private controlling terminal. Run '
      'mirror auth login claude --no-browser in an interactive terminal '
      'on this host.'
    )


class CommandCancelled(RuntimeError):
  def __init__(self) -> None:
    super().__init__(
      'Claude login cancelled. Run mirror auth status claude to check the saved login.'
    )


def parse_object(value: object, keys: set[str]) -> dict[str, object]:
  if not isinstance(value, dict):
    raise ProtocolError()
  result = cast(dict[str, object], value)
  if not result.keys() <= keys:
    raise ProtocolError()
  return result


def parse_enum(value: object, values: frozenset[str]) -> str:
  if not isinstance(value, str) or value not in values:
    raise ProtocolError()
  return value


def _safe_enum(value: object, values: frozenset[str], fallback: str) -> str:
  """An additive native enum cannot expose an arbitrary string or break recovery."""
  if not isinstance(value, str):
    raise ProtocolError()
  return value if value in values else fallback


def parse_timestamp(value: object) -> str | None:
  if value is None:
    return None
  if (
    not isinstance(value, str)
    or re.fullmatch(
      r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?(?:Z|\+00:00)',
      value,
    )
    is None
  ):
    raise ProtocolError()
  try:
    parsed = datetime.datetime.fromisoformat(value)
    if parsed.utcoffset() != datetime.timedelta(0):
      raise ValueError
    # Keep native nanoseconds intact for machine output; datetime validates
    # the calendar but only retains microseconds.
    return (
      value.removesuffix('+00:00') + 'Z' if value.endswith('+00:00') else value
    )
  except ValueError:
    raise ProtocolError() from None


def compatibility_profile(value: object) -> str:
  """Validate a native-owned public compatibility revision without copying it."""
  if (
    not isinstance(value, str)
    or not 1 <= len(value) <= 128
    or not all(
      char.isascii() and (char.isalnum() or char in '-_.:/') for char in value
    )
  ):
    raise ProtocolError()
  return value


def parse_identity(value: object) -> dict[str, str] | None:
  if value is None:
    return None
  record = parse_object(value, {'account_id', 'organization_id'})
  result: dict[str, str] = {}
  for key in ('account_id', 'organization_id'):
    item = record.get(key)
    try:
      identity = uuid.UUID(item) if isinstance(item, str) else None
    except ValueError:
      identity = None
    if identity is None or identity.int == 0:
      raise ProtocolError()
    result[key] = str(identity)
  return result


def safe_label(value: object) -> str | None:
  if value is None:
    return None
  if not isinstance(value, str) or len(value) > 512:
    raise ProtocolError()
  # Labels may come from the account service; never let them control a terminal.
  return ''.join(char for char in value if char.isprintable())


@dataclasses.dataclass(frozen=True)
class ClaudeError:
  kind: str
  recovery: str
  details: Mapping[str, object]

  @classmethod
  def parse(cls, value: object) -> ClaudeError:
    record = parse_object(
      value, {'schema_version', 'kind', 'retry', 'recovery', 'details'}
    )
    if (
      type(record.get('schema_version')) is not int
      or record['schema_version'] != 1
    ):
      raise ProtocolError()
    if record.get('retry') != 'terminal':
      raise ProtocolError()
    raw = parse_object(
      record.get('details'),
      {
        'http_status',
        'provider_code',
        'retry_at',
        'resets_at',
        'extra_usage',
        'limit_scope',
        'expected_identity',
        'current_identity',
        'configuration',
      },
    )
    details: dict[str, object] = {}
    status = raw.get('http_status')
    if status is not None and (
      type(status) is not int or not 100 <= status <= 599
    ):
      raise ProtocolError()
    details['http_status'] = status
    for key, choices in (
      ('provider_code', _PROVIDER_CODES),
      ('extra_usage', frozenset(('enabled', 'disabled', 'unknown'))),
      ('limit_scope', frozenset(('shared', 'model', 'unknown'))),
      ('configuration', _CONFIGURATIONS),
    ):
      details[key] = (
        None
        if raw.get(key) is None
        else _safe_enum(raw[key], choices, 'unknown')
      )
    for key in ('retry_at', 'resets_at'):
      details[key] = parse_timestamp(raw.get(key))
    for key in ('expected_identity', 'current_identity'):
      details[key] = parse_identity(raw.get(key))
    return cls(
      _safe_enum(record.get('kind'), _KINDS, 'unknown'),
      _safe_enum(record.get('recovery'), _RECOVERIES, 'none'),
      details,
    )

  def to_json(self) -> dict[str, object]:
    return {
      'schema_version': 1,
      'kind': self.kind,
      'retry': 'terminal',
      'recovery': self.recovery,
      'details': dict(self.details),
    }


@dataclasses.dataclass(frozen=True)
class AuthStatus:
  state: str
  identity: Mapping[str, str] | None
  display: Mapping[str, str | None] | None = dataclasses.field(repr=False)
  expires_at: str | None
  recovery: str
  error: ClaudeError | None

  @classmethod
  def parse(cls, value: object) -> AuthStatus:
    record = parse_object(
      value,
      {
        'schema_version',
        'provider',
        'source',
        'state',
        'identity',
        'display',
        'expires_at',
        'recovery',
        'error',
      },
    )
    if (
      type(record.get('schema_version')) is not int
      or record['schema_version'] != 1
      or record.get('provider') != PROVIDER
      or record.get('source') != 'Mirror login'
    ):
      raise ProtocolError()
    display = None
    if record.get('display') is not None:
      raw = parse_object(record['display'], {'email', 'organization_name'})
      display = {
        key: safe_label(raw.get(key)) for key in ('email', 'organization_name')
      }
    state = parse_enum(
      record.get('state'),
      frozenset(('logged_out', 'usable', 'refreshable', 'login_required')),
    )
    identity = parse_identity(record.get('identity'))
    expires_at = parse_timestamp(record.get('expires_at'))
    if state in ('usable', 'refreshable') and (
      identity is None or expires_at is None
    ):
      raise ProtocolError()
    return cls(
      state,
      identity,
      display,
      expires_at,
      parse_enum(record.get('recovery'), _RECOVERIES),
      ClaudeError.parse(record['error'])
      if record.get('error') is not None
      else None,
    )

  def to_json(self) -> dict[str, object]:
    return {
      'schema_version': 1,
      'provider': PROVIDER,
      'source': 'Mirror login',
      'state': self.state,
      'identity': dict(self.identity) if self.identity is not None else None,
      'display': dict(self.display) if self.display is not None else None,
      'expires_at': self.expires_at,
      'recovery': self.recovery,
      'error': self.error.to_json() if self.error is not None else None,
    }


@dataclasses.dataclass(frozen=True)
class Progress:
  phase: str
  notice: str | None


@dataclasses.dataclass(frozen=True)
class AuthorizationUrl:
  authorization_url: str = dataclasses.field(repr=False)
  expires_at: str


@dataclasses.dataclass(frozen=True)
class Completed:
  status: AuthStatus
  compatibility_profile: str


@dataclasses.dataclass(frozen=True)
class MetadataCompleted:
  metadata: Metadata
  compatibility_profile: str


@dataclasses.dataclass(frozen=True)
class Failed:
  error: ClaudeError
  phase: str
  commit_outcome: str | None
  compatibility_profile: str


type TerminalResult = Completed | MetadataCompleted | Failed
type Event = Progress | AuthorizationUrl | TerminalResult


class LoginTerminal(Protocol):
  """Private transient UI; never a chat, action or logging destination."""

  def notice(self, message: str) -> None: ...

  async def read_callback(self) -> str: ...


class NativeCommand:
  """One operation over dedicated pipes; EOF cancels native pending work."""

  def __init__(
    self,
    worker_command: Sequence[str],
    action: str,
    *,
    params: Mapping[str, object] | None = None,
  ) -> None:
    self._argv = [*worker_command, 'auth', '--stdio']
    self.action = action
    self._params = dict(params or {})
    self.request_id = str(uuid.uuid4())
    self._process: asyncio.subprocess.Process | None = None
    self._terminal = False
    self._result: TerminalResult | None = None
    self._cleanup_deadline: float | None = None
    self._cancel_requested = False
    self._deadline = 0.0
    self._events = 0
    self._compatibility_profile: str | None = None

  async def __aenter__(self) -> NativeCommand:
    try:
      self._process = await asyncio.create_subprocess_exec(
        *self._argv,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
        limit=MAX_OUTPUT_LINE + 1,
        start_new_session=True,
      )
      self._deadline = time.monotonic() + (
        330 if self.action == 'login' else 45
      )
      await self._send('request', {'params': self._params})
    except BaseException:
      await self._close()
      raise
    return self

  async def __aexit__(self, *_: object) -> None:
    # Finish cleanup even if a second UI cancellation arrives during reaping.
    task = asyncio.create_task(self._close())
    while not task.done():
      try:
        await asyncio.shield(task)
      except asyncio.CancelledError:
        continue
    task.result()

  async def _send(self, kind: str, fields: Mapping[str, object]) -> None:
    process = self._process
    if process is None or process.stdin is None:
      raise CommandInterrupted()
    envelope: dict[str, object] = {
      'schema_version': 1,
      'request_id': self.request_id,
      'type': kind,
    }
    if kind == 'request':
      envelope.update({'provider': PROVIDER, 'action': self.action})
    encoded = (json.dumps(envelope | dict(fields)) + '\n').encode()
    if len(encoded) > _MAX_INPUT_LINE:
      raise ProtocolError()
    try:
      process.stdin.write(encoded)
      await asyncio.wait_for(process.stdin.drain(), timeout=2)
    except (BrokenPipeError, ConnectionResetError, TimeoutError):
      raise CommandInterrupted() from None

  async def callback(self, value: str) -> None:
    """Submit transient input privately; never retain or echo it."""
    try:
      size = len(value.encode('utf-8'))
    except UnicodeError:
      raise ProtocolError() from None
    if size > MAX_CALLBACK:
      raise ProtocolError()
    await self._send('callback', {'value': value})

  async def read_event(self) -> Event:
    process = self._process
    if process is None or process.stdout is None or self._terminal:
      raise ProtocolError()
    try:
      raw = await asyncio.wait_for(
        process.stdout.readuntil(b'\n'),
        timeout=max(0, self._deadline - time.monotonic()),
      )
    except (asyncio.IncompleteReadError, TimeoutError):
      raise CommandInterrupted() from None
    except asyncio.LimitOverrunError:
      raise ProtocolError() from None
    self._events += 1
    # The native cap excludes the terminating newline.
    if len(raw) > MAX_OUTPUT_LINE + 1 or self._events > 1024:
      raise ProtocolError()
    try:
      value: object = json.loads(raw.decode('utf-8'))
    except (ValueError, UnicodeError):
      raise ProtocolError() from None
    event = self._parse_event(value)
    self._terminal = isinstance(event, (Completed, MetadataCompleted, Failed))
    if isinstance(event, (Completed, MetadataCompleted, Failed)):
      self._result = event
    return event

  def _parse_event(self, value: object) -> Event:
    common = {
      'schema_version',
      'request_id',
      'provider',
      'action',
      'type',
      'compatibility_profile',
    }
    record = parse_object(
      value,
      common
      | {
        'phase',
        'notice',
        'authorization_url',
        'expires_at',
        'result',
        'error',
        'commit_outcome',
      },
    )
    if (
      type(record.get('schema_version')) is not int
      or record['schema_version'] != 1
      or record.get('request_id') != self.request_id
      or record.get('provider') != PROVIDER
      or record.get('action') != self.action
    ):
      raise ProtocolError()
    profile = compatibility_profile(record.get('compatibility_profile'))
    if (
      self._compatibility_profile is not None
      and self._compatibility_profile != profile
    ):
      raise ProtocolError()
    self._compatibility_profile = profile
    kind = record.get('type')
    if kind == 'progress' and self.action == 'login':
      if not record.keys() <= common | {'phase', 'notice'}:
        raise ProtocolError()
      notice = record.get('notice')
      if notice is not None and notice != 'callback_rejected':
        raise ProtocolError()
      return Progress(
        parse_enum(record.get('phase'), _PHASES), cast(str | None, notice)
      )
    if kind == 'authorization_url' and self.action == 'login':
      if not record.keys() <= common | {'authorization_url', 'expires_at'}:
        raise ProtocolError()
      url = record.get('authorization_url')
      if not isinstance(url, str) or len(url) > MAX_CALLBACK:
        raise ProtocolError()
      try:
        parsed = urllib.parse.urlsplit(url)
        valid = (
          parsed.scheme == 'https'
          and parsed.netloc == 'claude.ai'
          and parsed.path == '/oauth/authorize'
          and not parsed.fragment
          and all(ord(char) > 32 and ord(char) < 127 for char in url)
        )
      except ValueError:
        valid = False
      expires = parse_timestamp(record.get('expires_at'))
      if not valid or expires is None:
        raise ProtocolError()
      return AuthorizationUrl(url, expires)
    if kind == 'completed':
      if not record.keys() <= common | {'result'}:
        raise ProtocolError()
      if self.action in ('models', 'model_context', 'usage'):
        from mirror.cli.claude_metadata import Metadata

        result = parse_object(record.get('result'), {self.action})
        metadata = Metadata.parse(self.action, result.get(self.action))
        expected = self._params.get('expected_identity')
        if expected is not None and metadata.identity != expected:
          raise ProtocolError()
        if self.action == 'model_context' and metadata.payload[
          'model'
        ] != self._params.get('model'):
          raise ProtocolError()
        return MetadataCompleted(metadata, profile)
      if self.action not in ('login', 'status', 'logout'):
        raise ProtocolError()
      result = parse_object(record.get('result'), {'status'})
      status = AuthStatus.parse(result.get('status'))
      if self.action == 'login' and status.state not in (
        'usable',
        'refreshable',
      ):
        raise ProtocolError()
      if self.action == 'logout' and status.state != 'logged_out':
        raise ProtocolError()
      return Completed(status, profile)
    if kind == 'error':
      if not record.keys() <= common | {'phase', 'error', 'commit_outcome'}:
        raise ProtocolError()
      outcome = record.get('commit_outcome')
      if self.action in ('models', 'model_context', 'usage') and (
        outcome is not None
        or record.get('phase') not in ('protocol', 'preparing')
      ):
        raise ProtocolError()
      if outcome is not None and outcome not in ('not_committed', 'unknown'):
        raise ProtocolError()
      return Failed(
        ClaudeError.parse(record.get('error')),
        parse_enum(record.get('phase'), _PHASES),
        cast(str | None, outcome),
        profile,
      )
    raise ProtocolError()

  async def cancel_result(self) -> TerminalResult:
    """Ask native to stop, retaining any already committed terminal result."""
    if self._result is not None:
      return self._result
    self._cleanup_deadline = time.monotonic() + 2
    # The operation deadline may have expired; the separate cleanup budget
    # still allows a buffered or concurrently committed terminal to arrive.
    self._deadline = self._cleanup_deadline
    self._cancel_requested = True
    try:
      await self._send('cancel', {})
    except CommandInterrupted:
      # A completed native process may have closed stdin while its terminal
      # event is still buffered on stdout. Always inspect that pipe first.
      pass
    while True:
      event = await self.read_event()
      if isinstance(event, (Completed, MetadataCompleted, Failed)):
        return event

  async def cancel_auth_result(self) -> Completed | Failed:
    result = await self.cancel_result()
    if not isinstance(result, (Completed, Failed)):
      raise ProtocolError()
    return result

  async def _close(self) -> None:
    process = self._process
    if process is None:
      return

    async def discard_output() -> None:
      if process.stdout is not None:
        while await process.stdout.read(65536):
          pass

    # Drain discarded output during shutdown so a full pipe cannot prevent
    # Process.wait from finishing, including after a framing-limit violation.
    draining = asyncio.create_task(discard_output())
    grace_deadline = self._cleanup_deadline or time.monotonic() + 2
    try:
      if (
        not self._terminal
        and not self._cancel_requested
        and process.returncode is None
      ):
        try:
          await self._send('cancel', {})
        except (OSError, CommandInterrupted):
          pass
      if process.stdin is not None:
        process.stdin.close()
      try:
        await asyncio.wait_for(
          process.wait(), timeout=max(0, grace_deadline - time.monotonic())
        )
      except TimeoutError:
        try:
          process.terminate()
        except ProcessLookupError:
          pass
        try:
          await asyncio.wait_for(process.wait(), timeout=1)
        except TimeoutError:
          try:
            process.kill()
          except ProcessLookupError:
            pass
          await process.wait()
    finally:
      draining.cancel()
      await asyncio.gather(draining, return_exceptions=True)
      self._process = None


class PrivateTerminal:
  """A real controlling terminal, with hidden input and synchronous restoration."""

  def __init__(self) -> None:
    self._fd: int | None = None
    self._original: list[int | list[bytes | int]] | None = None

  def __enter__(self) -> PrivateTerminal:
    try:
      self._fd = os.open('/dev/tty', os.O_RDWR | os.O_NOCTTY)
      original = termios.tcgetattr(self._fd)
      self._original = original
      changed = termios.tcgetattr(self._fd)
      changed[3] &= ~(termios.ECHO | termios.ECHONL)
      termios.tcflush(self._fd, termios.TCIFLUSH)
      termios.tcsetattr(self._fd, termios.TCSANOW, changed)
    except (OSError, termios.error):
      self.close()
      raise TerminalError() from None
    return self

  def __exit__(self, *_: object) -> None:
    self.close()

  def close(self) -> None:
    fd, self._fd = self._fd, None
    if fd is None:
      return
    try:
      if self._original is not None:
        try:
          termios.tcflush(fd, termios.TCIFLUSH)
        finally:
          termios.tcsetattr(fd, termios.TCSANOW, self._original)
      os.write(fd, b'\n')
    except (OSError, termios.error):
      raise TerminalError(cleanup=True) from None
    finally:
      os.close(fd)

  def notice(self, message: str) -> None:
    if self._fd is None:
      raise RuntimeError('Claude login terminal is closed.')
    os.write(self._fd, message.encode())

  async def read_callback(self) -> str:
    # Darwin's default KqueueSelector rejects actual /dev/tty with EINVAL.
    # Poll select without registering it with asyncio or leaving a reader thread.
    pending = bytearray()
    oversized = False
    while self._fd is not None:
      ready, _, _ = select.select([self._fd], [], [], 0)
      if ready:
        chunk = os.read(self._fd, MAX_CALLBACK + 1)
        if not chunk:
          raise CommandCancelled()
        if not oversized:
          pending.extend(chunk)
          oversized = len(pending) > MAX_CALLBACK
        if b'\n' in chunk or b'\r' in chunk:
          if oversized:
            self.notice(
              'Callback too long; paste the final callback again (input hidden).\n'
            )
            pending.clear()
            oversized = False
            continue
          try:
            return pending.decode().strip()
          except UnicodeError:
            self.notice(
              'Callback must be UTF-8; paste it again (input hidden).\n'
            )
            pending.clear()
      await asyncio.sleep(0.05)
    raise RuntimeError('Claude login terminal is closed.')


async def open_browser(url: str) -> bool:
  """Launch the system browser without allowing launcher output into logs."""
  process: asyncio.subprocess.Process | None = None
  try:
    process = await asyncio.create_subprocess_exec(
      'open' if sys.platform == 'darwin' else 'xdg-open',
      url,
      stdin=asyncio.subprocess.DEVNULL,
      stdout=asyncio.subprocess.DEVNULL,
      stderr=asyncio.subprocess.DEVNULL,
      start_new_session=True,
    )
    return await asyncio.wait_for(process.wait(), timeout=5) == 0
  except (OSError, TimeoutError):
    return False
  finally:
    if process is not None and process.returncode is None:
      try:
        process.kill()
      except ProcessLookupError:
        pass
      await process.wait()
