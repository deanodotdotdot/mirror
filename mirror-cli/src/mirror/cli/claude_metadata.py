"""Validated native metadata. No credentials, HTTP, caching or billing changes."""

from __future__ import annotations

import dataclasses
import math
from collections.abc import Mapping
from typing import cast

from mirror.cli import claude_auth as auth

_COMMON = {
  'schema_version',
  'provider',
  'identity',
  'state',
  'fetched_at',
  'age_seconds',
  'retry_at',
  'error',
}


def _record(value: object, keys: set[str]) -> dict[str, object]:
  record = auth.parse_object(value, keys)
  if record.keys() != keys:
    raise auth.ProtocolError()
  return record


def _integer(value: object, minimum: int = 0, maximum: int = 2**64 - 1) -> int:
  if type(value) is not int or not minimum <= value <= maximum:
    raise auth.ProtocolError()
  return value


def _number(value: object) -> int | float:
  if type(value) not in (int, float):
    raise auth.ProtocolError()
  number = cast(int | float, value)
  if not 0 <= number <= 9_007_199_254_740_991 or not math.isfinite(number):
    raise auth.ProtocolError()
  return number


def _label(value: object) -> str:
  label = auth.safe_label(value)
  if label is None or not label or len(label.encode()) > 256:
    raise auth.ProtocolError()
  return label


def model_id(value: object) -> str:
  if (
    not isinstance(value, str)
    or not 1 <= len(value) <= 128
    or not all(c.isascii() and (c.isalnum() or c in '-_.:') for c in value)
  ):
    raise auth.ProtocolError()
  return value


def params(
  action: str,
  expected_identity: Mapping[str, str] | None = None,
  *,
  model: str | None = None,
  explicit_context_window: int | None = None,
) -> dict[str, object]:
  """Identity comes from native status/session state, never Python storage."""
  identity = (
    auth.parse_identity(dict(expected_identity))
    if expected_identity is not None
    else None
  )
  result: dict[str, object] = {'expected_identity': identity}
  if action == 'model_context':
    if identity is None:
      raise auth.ProtocolError()
    result['model'] = model_id(model)
    result['explicit_context_window'] = (
      None
      if explicit_context_window is None
      else _integer(explicit_context_window, 1, 100_000_000)
    )
  elif action not in ('models', 'usage'):
    raise auth.ProtocolError()
  return result


@dataclasses.dataclass(frozen=True)
class Model:
  id: str
  display_name: str | None
  context_window: int | None

  @classmethod
  def parse(cls, value: object) -> Model:
    record = _record(value, {'id', 'display_name', 'context_window'})
    return cls(
      model_id(record['id']),
      None
      if record['display_name'] is None
      else _label(record['display_name']),
      None
      if record['context_window'] is None
      else _integer(record['context_window'], 1, 100_000_000),
    )


def _amount(value: object) -> dict[str, object] | None:
  if value is None:
    return None
  # Unknown units or malformed amount metadata are unavailable, never guessed.
  try:
    record = _record(value, {'value', 'unit', 'currency', 'decimal_places'})
    unit = auth.parse_enum(
      record['unit'], frozenset(('credits', 'currency_minor'))
    )
    currency = record['currency']
    if currency is not None and (
      not isinstance(currency, str)
      or len(currency) != 3
      or not currency.isascii()
      or not currency.isalpha()
      or not currency.isupper()
    ):
      raise auth.ProtocolError()
    return {
      'value': _number(record['value']),
      'unit': unit,
      'currency': currency,
      'decimal_places': None
      if record['decimal_places'] is None
      else _integer(record['decimal_places'], 0, 18),
    }
  except auth.ProtocolError:
    return None


def _window(value: object) -> dict[str, object]:
  record = _record(
    value,
    {
      'id',
      'scope',
      'model',
      'window_seconds',
      'used_percent',
      'resets_at',
      'is_active',
    },
  )
  active = record['is_active']
  if active is not None and type(active) is not bool:
    raise auth.ProtocolError()
  return {
    'id': _label(record['id']),
    'scope': auth.parse_enum(
      record['scope'], frozenset(('shared', 'model', 'unknown'))
    ),
    'model': None if record['model'] is None else _label(record['model']),
    'window_seconds': None
    if record['window_seconds'] is None
    else _integer(record['window_seconds'], 1),
    'used_percent': None
    if record['used_percent'] is None
    else _number(record['used_percent']),
    'resets_at': auth.parse_timestamp(record['resets_at']),
    'is_active': active,
  }


@dataclasses.dataclass(frozen=True)
class Metadata:
  action: str
  identity: Mapping[str, str]
  state: str
  fetched_at: str | None
  age_seconds: int | None
  retry_at: str | None
  error: auth.ClaudeError | None
  payload: Mapping[str, object]
  models: tuple[Model, ...] = ()

  @classmethod
  def parse(cls, action: str, value: object) -> Metadata:
    specific = {
      'models': {'source', 'models'},
      'model_context': {'model', 'context_window', 'provenance'},
      'usage': {'windows', 'extra_usage'},
    }
    if action not in specific:
      raise auth.ProtocolError()
    record = _record(value, _COMMON | specific[action])
    if (
      type(record['schema_version']) is not int
      or record['schema_version'] != 1
      or record['provider'] != auth.PROVIDER
    ):
      raise auth.ProtocolError()
    identity = auth.parse_identity(record['identity'])
    if identity is None:
      raise auth.ProtocolError()
    payload: dict[str, object]
    models: tuple[Model, ...] = ()
    if action == 'models':
      raw = record['models']
      if not isinstance(raw, list) or len(cast(list[object], raw)) > 1024:
        raise auth.ProtocolError()
      models = tuple(Model.parse(item) for item in cast(list[object], raw))
      if len({model.id for model in models}) != len(models):
        raise auth.ProtocolError()
      payload = {
        'source': auth.parse_enum(
          record['source'], frozenset(('remote', 'bundled'))
        ),
        'models': [dataclasses.asdict(model) for model in models],
      }
    elif action == 'model_context':
      payload = {
        'model': model_id(record['model']),
        'context_window': _integer(record['context_window'], 1, 100_000_000),
        'provenance': auth.parse_enum(
          record['provenance'],
          frozenset((
            'explicit',
            'model_metadata',
            'known_model_fallback',
            'conservative_fallback',
          )),
        ),
      }
    else:
      raw = record['windows']
      if not isinstance(raw, list) or len(cast(list[object], raw)) > 128:
        raise auth.ProtocolError()
      extra = _record(
        record['extra_usage'], {'state', 'source', 'used', 'limit'}
      )
      payload = {
        'windows': [_window(item) for item in cast(list[object], raw)],
        'extra_usage': {
          'state': auth.parse_enum(
            extra['state'], frozenset(('enabled', 'disabled', 'unknown'))
          ),
          'source': auth.parse_enum(
            extra['source'], frozenset(('legacy', 'spend', 'unreported'))
          ),
          'used': _amount(extra['used']),
          'limit': _amount(extra['limit']),
        },
      }
    return cls(
      action,
      identity,
      auth.parse_enum(
        record['state'],
        frozenset(('fresh', 'cached', 'stale', 'offline', 'unavailable')),
      ),
      auth.parse_timestamp(record['fetched_at']),
      None
      if record['age_seconds'] is None
      else _integer(record['age_seconds']),
      auth.parse_timestamp(record['retry_at']),
      None
      if record['error'] is None
      else auth.ClaudeError.parse(record['error']),
      payload,
      models,
    )

  def to_json(self) -> dict[str, object]:
    return {
      'schema_version': 1,
      'provider': auth.PROVIDER,
      'identity': dict(self.identity),
      'state': self.state,
      'fetched_at': self.fetched_at,
      'age_seconds': self.age_seconds,
      'retry_at': self.retry_at,
      'error': None if self.error is None else self.error.to_json(),
      **self.payload,
    }


def amount_text(value: object) -> str:
  amount = _amount(value)
  if amount is None:
    return 'unavailable'
  # Keep the reported unit verbatim. Even a reported currency/scale does not
  # establish a conversion from credits, or a shared scale between amounts.
  parts = [f'{amount["value"]} {amount["unit"]}']
  if amount['currency'] is not None:
    parts.append(f'currency {amount["currency"]}')
  if amount['decimal_places'] is not None:
    parts.append(f'decimal places {amount["decimal_places"]}')
  return ' · '.join(parts)


def text(
  result: Metadata, profile: str, *, account_scope: str = 'current Mirror login'
) -> str:
  from mirror.claude_commands import error_text, local_time

  label = (
    'model candidates'
    if result.action == 'models'
    else result.action.replace('_', ' ')
  )
  lines = [
    f'Claude subscription {label}: {result.state}',
    f'Account ({account_scope}): {result.identity["account_id"]}',
    f'Workspace: {result.identity["organization_id"]}',
    f'Fetched: {local_time(result.fetched_at)}',
    f'Age: {result.age_seconds} seconds'
    if result.age_seconds is not None
    else 'Age: unavailable',
  ]
  if result.retry_at is not None:
    lines.append(f'Retry after: {local_time(result.retry_at)}')
  if result.error is not None:
    lines.append(error_text(result.error, profile, metadata=True))
  if result.action == 'models':
    lines.extend((
      f'Source: {result.payload["source"]}',
      'Candidates do not guarantee account access.',
    ))
  elif result.action == 'model_context':
    lines.append(
      f'Context: {result.payload["context_window"]} · {result.payload["provenance"]}'
    )
  else:
    windows = cast(list[dict[str, object]], result.payload['windows'])
    if not windows:
      lines.append('Usage windows: unavailable')
    for window in windows:
      percent = window['used_percent']
      duration = window['window_seconds']
      active = window['is_active']
      lines.append(
        f'{window["id"]} · scope {window["scope"]} · model {window["model"] or "unavailable"}'
        f' · window {str(duration) + " seconds" if duration is not None else "unavailable"}'
        f' · used {str(percent) + "%" if percent is not None else "unavailable"}'
        f' · active {str(active).lower() if active is not None else "unavailable"}'
        f' · resets {local_time(cast(str | None, window["resets_at"]))}'
      )
    extra = cast(dict[str, object], result.payload['extra_usage'])
    lines.extend((
      f'Account extra usage: {extra["state"]} · source {extra["source"]}',
      f'Reported extra usage used: {amount_text(extra["used"])}',
      f'Reported extra usage limit: {amount_text(extra["limit"])}',
    ))
  return '\n'.join(lines)
