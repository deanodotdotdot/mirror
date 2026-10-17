"""Native metadata schema, billing units, identity fences and on-demand output."""

from __future__ import annotations

import asyncio
import copy
import json
import os
import pathlib
import sys
import time
from unittest import mock

import pytest

from mirror import claude_commands
from mirror.cli import claude_auth as auth
from mirror.cli import claude_metadata as metadata

IDENTITY = {
  'account_id': '11111111-1111-4111-8111-111111111111',
  'organization_id': '22222222-2222-4222-8222-222222222222',
}
COMMON = {
  'schema_version': 1,
  'provider': 'anthropic-oauth',
  'identity': IDENTITY,
  'state': 'fresh',
  'fetched_at': '2026-10-17T18:00:00Z',
  'age_seconds': 0,
  'retry_at': None,
  'error': None,
}
USAGE = COMMON | {
  'windows': [
    {
      'id': 'five_hour',
      'scope': 'shared',
      'model': None,
      'window_seconds': 18000,
      'used_percent': 125.5,
      'resets_at': '2026-10-17T23:00:00Z',
      'is_active': True,
    },
    {
      'id': 'seven_day_opus',
      'scope': 'model',
      'model': 'Opus family',
      'window_seconds': 604800,
      'used_percent': None,
      'resets_at': None,
      'is_active': False,
    },
    {
      'id': 'provider_window',
      'scope': 'unknown',
      'model': None,
      'window_seconds': None,
      'used_percent': 0,
      'resets_at': None,
      'is_active': None,
    },
  ],
  'extra_usage': {
    'state': 'disabled',
    'source': 'legacy',
    'used': {
      'value': 12345.0,
      'unit': 'credits',
      'currency': 'EUR',
      'decimal_places': 3,
    },
    'limit': None,
  },
}
CATALOG = COMMON | {
  'source': 'remote',
  'models': [
    {'id': 'claude-example', 'display_name': None, 'context_window': None},
    {
      'id': 'claude-other',
      'display_name': 'Other candidate',
      'context_window': 200000,
    },
  ],
}
CONTEXT = COMMON | {
  'model': 'claude-example',
  'context_window': 200000,
  'provenance': 'conservative_fallback',
}
ERROR = {
  'schema_version': 1,
  'kind': 'auth_temporarily_unavailable',
  'retry': 'terminal',
  'recovery': 'retry_later',
  'details': {},
}


@pytest.fixture(autouse=True)
def isolated_home(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.setenv('HOME', str(tmp_path))


@pytest.mark.parametrize(
  'state', ['fresh', 'cached', 'stale', 'offline', 'unavailable']
)
@pytest.mark.parametrize(
  'action,payload',
  [('models', CATALOG), ('model_context', CONTEXT), ('usage', USAGE)],
)
def test_metadata_preserves_states_nulls_and_utc(
  action: str, payload: dict, state: str
) -> None:
  raw = payload | {
    'state': state,
    'age_seconds': 3601 if state == 'stale' else None,
  }
  result = metadata.Metadata.parse(action, raw)
  assert result.to_json() == raw
  assert result.identity == IDENTITY


def test_usage_human_times_local_and_raw_amounts(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  previous = os.environ.get('TZ')
  monkeypatch.setenv('TZ', 'America/New_York')
  time.tzset()
  try:
    result = metadata.Metadata.parse('usage', USAGE)
    rendered = metadata.text(result, 'fixture-v1')
    assert '2026-10-17 14:00:00 EDT (UTC-0400)' in rendered
    assert '2026-10-17 19:00:00 EDT (UTC-0400)' in rendered
    assert '125.5%' in rendered
    assert 'active false' in rendered
    assert 'used unavailable' in rendered
    assert 'used 0%' in rendered
    assert '12345.0 credits · currency EUR · decimal places 3' in rendered
    assert 'Account extra usage: disabled' in rendered
    assert 'limit: unavailable' in rendered
    assert (
      'USD' not in rendered
      and '$' not in rendered
      and 'unlimited' not in rendered
    )
    assert result.to_json()['fetched_at'] == '2026-10-17T18:00:00Z'
  finally:
    if previous is None:
      os.environ.pop('TZ', None)
    else:
      os.environ['TZ'] = previous
    time.tzset()


@pytest.mark.parametrize(
  'change',
  [
    {'unit': 'unknown'},
    {'value': -1},
    {'value': float('nan')},
    {'value': True},
    {'decimal_places': -1},
    {'decimal_places': 19},
    {'currency': 'not a currency'},
    {'extra': 'private-canary'},
  ],
)
def test_malformed_amount_is_unavailable_without_guessing(change: dict) -> None:
  raw = copy.deepcopy(USAGE)
  raw['extra_usage']['used'].update(change)
  result = metadata.Metadata.parse('usage', raw)
  assert result.to_json()['extra_usage']['used'] is None
  assert 'Reported extra usage used: unavailable' in metadata.text(
    result, 'fixture-v1'
  )


def test_distinct_spend_units_scales_and_missing_currency_are_not_converted() -> (
  None
):
  raw = USAGE | {
    'extra_usage': {
      'state': 'enabled',
      'source': 'spend',
      'used': {
        'value': 159,
        'unit': 'currency_minor',
        'currency': 'JPY',
        'decimal_places': 0,
      },
      'limit': {
        'value': 2000,
        'unit': 'currency_minor',
        'currency': None,
        'decimal_places': None,
      },
    }
  }
  result = metadata.Metadata.parse('usage', raw)
  assert result.to_json() == raw
  rendered = metadata.text(result, 'fixture-v1')
  assert '159 currency_minor · currency JPY · decimal places 0' in rendered
  assert '2000 currency_minor' in rendered
  assert '$' not in rendered and 'USD' not in rendered


@pytest.mark.parametrize(
  'change',
  [
    {'schema_version': True},
    {'schema_version': 2},
    {'provider': 'anthropic-messages'},
    {'identity': None},
    {'state': 'private-canary'},
    {'fetched_at': '2026-10-17T19:00:00-04:00'},
    {'age_seconds': -1},
    {'age_seconds': True},
    {'retry_at': 'not a date'},
    {'unexpected': 'private-canary'},
  ],
)
def test_malformed_metadata_fails_safely(change: dict) -> None:
  with pytest.raises(auth.ProtocolError) as error:
    metadata.Metadata.parse('usage', USAGE | change)
  assert 'private-canary' not in str(error.value)


@pytest.mark.parametrize('field', list(COMMON))
def test_common_fields_are_required_even_when_nullable(field: str) -> None:
  raw = USAGE.copy()
  del raw[field]
  with pytest.raises(auth.ProtocolError):
    metadata.Metadata.parse('usage', raw)


@pytest.mark.parametrize(
  'provenance',
  [
    'explicit',
    'model_metadata',
    'known_model_fallback',
    'conservative_fallback',
  ],
)
def test_context_provenance_is_retained(provenance: str) -> None:
  raw = CONTEXT | {'provenance': provenance}
  assert metadata.Metadata.parse('model_context', raw).to_json() == raw


def test_metadata_params_require_native_identity_for_context() -> None:
  assert metadata.params('usage') == {'expected_identity': None}
  assert metadata.params('models', IDENTITY) == {'expected_identity': IDENTITY}
  assert metadata.params(
    'model_context',
    IDENTITY,
    model='claude-example',
    explicit_context_window=120000,
  ) == {
    'expected_identity': IDENTITY,
    'model': 'claude-example',
    'explicit_context_window': 120000,
  }
  with pytest.raises(auth.ProtocolError):
    metadata.params('model_context', model='claude-example')
  with pytest.raises(auth.ProtocolError):
    metadata.params(
      'model_context',
      IDENTITY,
      model='claude-example',
      explicit_context_window=0,
    )


def envelope(command: auth.NativeCommand, payload: dict) -> dict:
  return {
    'schema_version': 1,
    'provider': 'anthropic-oauth',
    'request_id': command.request_id,
    'action': command.action,
    'type': 'completed',
    'compatibility_profile': 'fixture-v1',
    'result': {command.action: payload},
  }


@pytest.mark.parametrize(
  'change',
  [
    {'schema_version': 2},
    {'provider': 'anthropic-messages'},
    {'request_id': 'wrong'},
    {'action': 'status'},
    {'type': 'progress', 'phase': 'preparing'},
    {'result': {'status': USAGE}},
  ],
)
def test_metadata_envelope_is_strict(change: dict) -> None:
  command = auth.NativeCommand(['unused'], 'usage')
  with pytest.raises(auth.ProtocolError):
    command._parse_event(envelope(command, USAGE) | change)


def test_metadata_response_identity_and_model_must_match_request() -> None:
  command = auth.NativeCommand(
    ['unused'], 'models', params=metadata.params('models', IDENTITY)
  )
  assert isinstance(
    command._parse_event(envelope(command, CATALOG)), auth.MetadataCompleted
  )
  other = dict(IDENTITY, account_id='33333333-3333-4333-8333-333333333333')
  with pytest.raises(auth.ProtocolError):
    command._parse_event(envelope(command, CATALOG | {'identity': other}))
  context = auth.NativeCommand(
    ['unused'],
    'model_context',
    params=metadata.params('model_context', IDENTITY, model='claude-example'),
  )
  with pytest.raises(auth.ProtocolError):
    context._parse_event(envelope(context, CONTEXT | {'model': 'claude-other'}))


@pytest.mark.asyncio
@pytest.mark.parametrize(
  'action,payload',
  [('models', CATALOG), ('model_context', CONTEXT), ('usage', USAGE)],
)
async def test_private_native_metadata_pipes(
  action: str, payload: dict, tmp_path: pathlib.Path
) -> None:
  requested = metadata.params(action, IDENTITY, model='claude-example')
  native = tmp_path / 'fixture.py'
  native.write_text(f"""
import json, select, sys
assert sys.argv[1:] == ['auth', '--stdio']
request = json.loads(sys.stdin.readline())
assert request['params'] == {requested!r}
assert not select.select([sys.stdin], [], [], 0.02)[0]
base = {{key: request[key] for key in ('schema_version','provider','request_id','action')}}
print(json.dumps(base | {{'type':'completed','compatibility_profile':'fixture-v1','result':{{request['action']:{payload!r}}}}}), flush=True)
""")
  async with auth.NativeCommand(
    [sys.executable, str(native)], action, params=requested
  ) as command:
    result = await command.read_event()
    assert isinstance(result, auth.MetadataCompleted)
    assert result.metadata.to_json() == payload
  assert not (tmp_path / '.mirror').exists()


@pytest.mark.parametrize('machine', [False, True])
@pytest.mark.parametrize('state', ['fresh', 'stale', 'unavailable'])
def test_usage_cli_reports_degraded_state_and_error(
  machine: bool, state: str, capsys: pytest.CaptureFixture[str]
) -> None:
  payload = USAGE | {'state': state}
  if state != 'fresh':
    payload |= {
      'error': ERROR,
      'age_seconds': 61,
      'retry_at': '2026-10-17T18:01:00Z',
    }
  if state == 'unavailable':
    payload |= {
      'fetched_at': None,
      'age_seconds': None,
      'windows': [],
      'extra_usage': {
        'state': 'unknown',
        'source': 'unreported',
        'used': None,
        'limit': None,
      },
    }
  result = auth.MetadataCompleted(
    metadata.Metadata.parse('usage', payload), 'fixture-v1'
  )
  with mock.patch.object(
    claude_commands, '_execute', mock.AsyncMock(return_value=result)
  ):
    code = claude_commands.run(
      ['usage', 'claude'] + (['--json'] if machine else [])
    )
  assert code == (1 if state == 'unavailable' else 0)
  captured = capsys.readouterr()
  assert not captured.err
  if machine:
    decoded = json.loads(captured.out)
    assert decoded['state'] == state
    assert decoded['compatibility_profile'] == 'fixture-v1'
    assert decoded['identity'] == IDENTITY
    assert decoded['fetched_at'] == payload['fetched_at']
  else:
    assert f'usage: {state}' in captured.out
    assert 'current Mirror login' in captured.out
    if state == 'unavailable':
      assert '0%' not in captured.out and 'unlimited' not in captured.out
      assert 'Usage windows: unavailable' in captured.out
    if state != 'fresh':
      assert 'temporarily unavailable' in captured.out


@pytest.mark.asyncio
async def test_installed_native_metadata_requires_login_without_network(
  tmp_path: pathlib.Path,
) -> None:
  executable = os.environ.get('MIRROR_TEST_NATIVE_WORKER')
  if not executable:
    pytest.skip(
      'Set MIRROR_TEST_NATIVE_WORKER for actual native interoperability'
    )
  for action in ('models', 'model_context', 'usage'):
    requested = metadata.params(
      action,
      IDENTITY if action == 'model_context' else None,
      model='claude-example',
    )
    async with auth.NativeCommand(
      [executable], action, params=requested
    ) as command:
      result = await asyncio.wait_for(command.read_event(), 3)
      assert isinstance(result, auth.Failed)
      assert result.error.kind == 'login_required'
      assert result.phase == 'preparing'
      assert result.commit_outcome is None
      assert result.compatibility_profile.startswith('mirror-phase0-')
  assert not list(tmp_path.rglob('metadata*.json'))


@pytest.mark.parametrize('value', [10**400, float('inf'), -1, True])
def test_usage_numbers_are_bounded_without_parser_exceptions(
  value: object,
) -> None:
  raw = copy.deepcopy(USAGE)
  raw['windows'][0]['used_percent'] = value
  with pytest.raises(auth.ProtocolError):
    metadata.Metadata.parse('usage', raw)


@pytest.mark.parametrize(
  'phase,outcome',
  [
    ('committing', None),
    ('preparing', 'unknown'),
    ('preparing', 'not_committed'),
  ],
)
def test_metadata_errors_do_not_claim_auth_commit_phase(
  phase: str, outcome: str | None
) -> None:
  command = auth.NativeCommand(['unused'], 'usage')
  raw = envelope(command, USAGE)
  del raw['result']
  raw.update(type='error', error=ERROR, phase=phase, commit_outcome=outcome)
  with pytest.raises(auth.ProtocolError):
    command._parse_event(raw)


def test_actual_cli_usage_missing_login_json_is_safe_and_has_no_chat_side_effects(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  capsys: pytest.CaptureFixture[str],
) -> None:
  executable = os.environ.get('MIRROR_TEST_NATIVE_WORKER')
  if not executable:
    pytest.skip(
      'Set MIRROR_TEST_NATIVE_WORKER for actual native interoperability'
    )
  monkeypatch.setattr(claude_commands.worker, 'command', lambda: [executable])
  assert claude_commands.run(['usage', 'claude', '--json']) == 1
  output = capsys.readouterr()
  assert not output.err
  result = json.loads(output.out)
  assert (
    result['schema_version'] == 1 and result['provider'] == 'anthropic-oauth'
  )
  assert result['error']['kind'] == 'login_required'
  assert result['compatibility_profile'].startswith('mirror-phase0-')
  assert not list(tmp_path.rglob('*.db'))
  assert not list(tmp_path.rglob('*.log'))


def test_metadata_json_preserves_native_timestamp_precision() -> None:
  stamp = '2026-10-17T18:00:00.123456789Z'
  result = metadata.Metadata.parse('usage', USAGE | {'fetched_at': stamp})
  assert result.to_json()['fetched_at'] == stamp
  assert auth.parse_timestamp(stamp.replace('Z', '+00:00')) == stamp
  for invalid in (
    '2026-10-17 18:00:00Z',
    '20261017T180000Z',
    '2026-10-17T18:00:00',
    '2026-13-17T18:00:00Z',
  ):
    with pytest.raises(auth.ProtocolError):
      auth.parse_timestamp(invalid)
