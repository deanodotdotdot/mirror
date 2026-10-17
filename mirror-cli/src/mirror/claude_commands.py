"""Reserved pre-chat Claude commands, without importing the chat application."""

from __future__ import annotations

import argparse
import asyncio
import datetime
import json
import sys
from collections.abc import Sequence
from typing import NoReturn, override

from mirror import worker
from mirror.cli import claude_auth, claude_metadata


class _Parser(argparse.ArgumentParser):
  @override
  def error(self, message: str) -> NoReturn:
    del message  # Argument errors must not echo a mistakenly pasted callback.
    super().error('invalid Claude command arguments; use --help for usage')


def parse_args(arguments: Sequence[str]) -> argparse.Namespace:
  """Parse only the reserved commands; never fall back to an exec prompt."""
  parser = _Parser(prog='mirror', allow_abbrev=False)
  parser.add_argument(
    '--no-update', action='store_true', help=argparse.SUPPRESS
  )
  commands = parser.add_subparsers(dest='command', required=True)
  auth = commands.add_parser(
    'auth', help='Manage Mirror’s Claude subscription login', allow_abbrev=False
  )
  actions = auth.add_subparsers(dest='action', required=True)
  for action, description in (
    ('login', 'Sign in with your Claude subscription'),
    ('status', 'Inspect local login state without network access'),
    ('logout', 'Remove Mirror’s saved Claude login'),
  ):
    command = actions.add_parser(action, help=description, allow_abbrev=False)
    command.add_argument('provider', choices=['claude'])
    if action == 'login':
      command.add_argument(
        '--no-browser',
        action='store_true',
        help='Open the displayed URL yourself, including from another computer',
      )
    if action == 'status':
      command.add_argument(
        '--json',
        action='store_true',
        help='Print versioned JSON with UTC timestamps',
      )
  usage = commands.add_parser(
    'usage',
    help='Fetch Claude subscription usage on demand',
    allow_abbrev=False,
  )
  usage.add_argument('provider', choices=['claude'])
  usage.add_argument(
    '--json',
    action='store_true',
    help='Print versioned JSON with UTC timestamps',
  )
  usage.set_defaults(action='usage')
  return parser.parse_args([arg for arg in arguments if arg != '--no-update'])


def local_time(value: str | None) -> str:
  """Human output names the local timezone; null remains unavailable."""
  if value is None:
    return 'unavailable'
  return (
    datetime.datetime
    .fromisoformat(value)
    .astimezone()
    .strftime('%Y-%m-%d %H:%M:%S %Z (UTC%z)')
  )


def error_text(
  error: claude_auth.ClaudeError,
  compatibility_profile: str | None = None,
  *,
  metadata: bool = False,
) -> str:
  """Render reviewed fixed text, never raw native/provider messages."""
  messages = {
    'login_required': 'Claude subscription login is required.',
    'login_expired': 'Claude subscription login has expired.',
    'login_cancelled': 'Claude login cancelled.',
    'login_timeout': 'Claude login timed out.',
    'credential_storage_error': 'Could not safely read or save Mirror’s Claude login.',
    'account_changed': 'This conversation is bound to a different Claude account or workspace.',
    'subscription_limited': 'The Claude subscription request reached a reported usage limit.',
    'rate_limited': 'Claude could not accept the request because of a rate limit.',
    'model_unavailable': 'The selected model is unavailable for this Claude subscription.',
    'access_denied': 'This Claude account cannot access the selected model or operation.',
    'auth_temporarily_unavailable': (
      'Claude subscription metadata is temporarily unavailable.'
      if metadata
      else 'Claude subscription authentication is temporarily unavailable.'
    ),
    'compatibility_error': 'This Mirror build needs an updated Claude compatibility profile.',
    'invalid_configuration': 'Claude subscription configuration is invalid.',
    'partial_stream_error': 'Claude output is incomplete; the request was not replayed.',
    'request_cancelled': 'Claude request cancelled.',
  }
  parts = [messages.get(error.kind, 'Claude subscription command failed.')]
  configuration = error.details.get('configuration')
  if compatibility_profile is not None and (
    error.kind == 'compatibility_error'
    or configuration in ('unknown_schema', 'unknown_profile')
  ):
    parts.append(f'Compatibility profile: {compatibility_profile}.')
  if configuration == 'api_key_not_allowed':
    parts.append('Remove --api-key when selecting Claude subscription.')
  elif configuration == 'endpoint_not_allowed':
    parts.append(
      'Use the default Anthropic endpoint; unset ANTHROPIC_BASE_URL or select Anthropic API.'
    )
  elif configuration == 'unsupported_surface':
    parts.append('Claude subscription is unsupported on this launch surface.')
  elif configuration == 'not_implemented':
    parts.append(
      'Claude subscription support is unavailable in this build; upgrade Mirror.'
    )
  elif configuration == 'invalid_request_options':
    parts.append(
      'Use /effort to choose a supported reasoning preset and check the selected model and request options.'
    )
  for key, label in (
    ('retry_at', 'Retry after'),
    ('resets_at', 'Reported reset'),
  ):
    value = error.details.get(key)
    if isinstance(value, str):
      parts.append(f'{label}: {local_time(value)}.')
  scope = error.details.get('limit_scope')
  if scope in ('shared', 'model', 'unknown'):
    parts.append(f'Reported limit scope: {scope}.')
  extra_usage = error.details.get('extra_usage')
  if extra_usage in ('enabled', 'disabled', 'unknown'):
    parts.append(f'Account extra usage: {extra_usage}.')
  if error.recovery == 'login':
    parts.append('Run mirror auth login claude.')
  elif error.recovery == 'reconnect':
    parts.append('Use /reconnect claude in the idle conversation.')
  elif error.recovery == 'repair_storage':
    parts.append('Repair Mirror’s private credential storage, then retry.')
  elif error.recovery == 'retry_later':
    parts.append('Retry later with the selected account and provider.')
  elif error.recovery == 'upgrade':
    parts.append('Upgrade Mirror and retry.')
  return ' '.join(parts)


def status_text(
  status: claude_auth.AuthStatus,
  compatibility_profile: str | None = None,
) -> str:
  states = {
    'logged_out': 'logged out',
    'usable': 'signed in',
    'refreshable': 'login saved; automatic renewal available',
    'login_required': 'sign-in required',
  }
  lines = [
    f'Claude subscription: {states[status.state]}',
    'Source: Mirror login',
  ]
  display = status.display or {}
  identity = status.identity or {}
  lines.append(
    f'Account: {display.get("email") or identity.get("account_id") or "unavailable"}'
  )
  lines.append(
    f'Workspace: {display.get("organization_name") or identity.get("organization_id") or "unavailable"}'
  )
  lines.append(f'Expires: {local_time(status.expires_at)}')
  if status.error is not None:
    lines.append(error_text(status.error, compatibility_profile))
  elif status.recovery == 'login':
    lines.append('Run mirror auth login claude.')
  return '\n'.join(lines)


async def login(
  command: claude_auth.NativeCommand,
  terminal: claude_auth.LoginTerminal,
  *,
  no_browser: bool,
) -> claude_auth.Completed | claude_auth.Failed:
  reader = asyncio.create_task(command.read_event())
  callback: asyncio.Task[str] | None = None
  instructions = ''
  try:
    while True:
      pending = [reader] if callback is None else [reader, callback]
      done, _ = await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)
      # A delivered terminal result wins a simultaneous input/cancellation race.
      if reader in done:
        event = reader.result()
        if isinstance(event, (claude_auth.Completed, claude_auth.Failed)):
          return event
        if isinstance(event, claude_auth.AuthorizationUrl):
          instructions = (
            'Sign into your existing Claude subscription.\n'
            'Leave this command running while you open the URL:\n'
            f'{event.authorization_url}\n'
            'On another computer, a localhost browser error is expected. Copy '
            'the final callback URL from the address bar and paste it here, '
            'then press Enter. Input is hidden; no characters will appear. '
            'Ctrl-C cancels.\n'
          )
          terminal.notice(instructions)
          if callback is None:
            callback = asyncio.create_task(terminal.read_callback())
          if not no_browser and not await claude_auth.open_browser(
            event.authorization_url
          ):
            terminal.notice(
              'Browser launch failed; open the URL manually.\n' + instructions
            )
        elif (
          isinstance(event, claude_auth.Progress)
          and event.notice == 'callback_rejected'
        ):
          terminal.notice(
            'Callback rejected; paste the final callback URL again and press Enter (input hidden).\n'
            + instructions
          )
        reader = asyncio.create_task(command.read_event())
      if callback is not None and callback in done:
        value = callback.result()
        await command.callback(value)
        value = ''
        callback = asyncio.create_task(terminal.read_callback())
  finally:
    reader.cancel()
    if callback is not None:
      callback.cancel()
    await asyncio.gather(
      reader,
      *([callback] if callback is not None else []),
      return_exceptions=True,
    )


async def _execute(
  args: argparse.Namespace,
) -> claude_auth.TerminalResult:
  async with claude_auth.NativeCommand(
    worker.command(), args.action
  ) as command:
    try:
      if args.action == 'login':
        # Restore the terminal before bounded cancellation and native reaping.
        with claude_auth.PrivateTerminal() as terminal:
          return await login(command, terminal, no_browser=args.no_browser)
      event = await command.read_event()
      if not isinstance(
        event,
        (
          claude_auth.Completed,
          claude_auth.MetadataCompleted,
          claude_auth.Failed,
        ),
      ):
        raise claude_auth.ProtocolError()
      return event
    except (
      asyncio.CancelledError,
      claude_auth.CommandCancelled,
      claude_auth.CommandInterrupted,
    ) as interrupted:
      try:
        return await command.cancel_result()
      except claude_auth.CommandInterrupted:
        if isinstance(interrupted, claude_auth.CommandInterrupted):
          raise
        raise claude_auth.CommandCancelled() from None


def run(arguments: Sequence[str] | None = None) -> int:
  """Run without logs, workspace trust, extension loading or chat databases."""
  args = parse_args(sys.argv[1:] if arguments is None else arguments)
  machine = bool(getattr(args, 'json', False))
  try:
    result = asyncio.run(_execute(args))
  except (KeyboardInterrupt, claude_auth.CommandCancelled):
    if machine:
      print(
        json.dumps({
          'schema_version': 1,
          'provider': claude_auth.PROVIDER,
          'compatibility_profile': None,
          'error': {'kind': 'request_cancelled', 'recovery': 'none'},
          'commit_outcome': 'unknown',
        })
      )
    else:
      print(
        'mirror: Claude command interrupted. Run mirror auth status claude '
        'to check the saved login before retrying.',
        file=sys.stderr,
      )
    return 130
  except (OSError, RuntimeError) as error:
    safe = (
      str(error)
      if isinstance(
        error,
        (
          claude_auth.ProtocolError,
          claude_auth.CommandInterrupted,
          claude_auth.TerminalError,
        ),
      )
      else 'Could not complete the private Claude command. Run mirror auth status claude to check the saved login before retrying.'
    )
    if machine:
      print(
        json.dumps({
          'schema_version': 1,
          'provider': claude_auth.PROVIDER,
          'compatibility_profile': None,
          'error': {
            'kind': 'invalid_configuration'
            if isinstance(error, claude_auth.ProtocolError)
            else 'auth_temporarily_unavailable',
            'recovery': 'check_configuration'
            if isinstance(error, claude_auth.ProtocolError)
            else 'retry_later',
          },
          'commit_outcome': 'unknown',
        })
      )
    else:
      print(f'mirror: {safe}', file=sys.stderr)
    return 1
  if isinstance(result, claude_auth.Failed):
    if machine:
      print(
        json.dumps({
          'schema_version': 1,
          'provider': claude_auth.PROVIDER,
          'compatibility_profile': result.compatibility_profile,
          'error': result.error.to_json(),
          'commit_outcome': result.commit_outcome,
        })
      )
    else:
      message = error_text(result.error, result.compatibility_profile)
      if result.commit_outcome == 'unknown':
        message += ' Run mirror auth status claude to check the saved login before retrying.'
      print(f'mirror: {message}', file=sys.stderr)
    return (
      130
      if result.error.kind in ('login_cancelled', 'request_cancelled')
      else 1
    )
  if isinstance(result, claude_auth.MetadataCompleted):
    if machine:
      print(
        json.dumps(
          result.metadata.to_json()
          | {
            'compatibility_profile': result.compatibility_profile,
          }
        )
      )
    else:
      print(claude_metadata.text(result.metadata, result.compatibility_profile))
    # Native successfully delivered a degraded report; retain its explicit state
    # in JSON and a nonzero shell result when there is no usable report at all.
    return 1 if result.metadata.state == 'unavailable' else 0
  if machine:
    print(
      json.dumps(
        result.status.to_json()
        | {
          'compatibility_profile': result.compatibility_profile,
        }
      )
    )
  else:
    print(status_text(result.status, result.compatibility_profile))
  return 0


if __name__ == '__main__':
  raise SystemExit(run())
