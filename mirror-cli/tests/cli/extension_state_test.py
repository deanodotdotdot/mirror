"""State-only updates must not traverse extension configuration."""

import copy
from typing import Self

from mirror import config
from mirror.cli import extension_state


class _Configuration(dict[str, config.JsonValue]):
  def __init__(self, copies: list[None]) -> None:
    super().__init__({'nested': ['original']})
    self.copies = copies

  def __deepcopy__(self, memo: dict[int, object]) -> Self:
    self.copies.append(None)
    copied = type(self)(self.copies)
    copied.update(copy.deepcopy(dict(self), memo))
    return copied


def test_updated_reuses_identical_state_including_none() -> None:
  states = extension_state.ExtensionStates({'plugin': None})
  assert states.updated('plugin', None) is states
  assert 'new-plugin' in states.updated('new-plugin', None)


def test_updated_shares_owned_configuration_without_copying() -> None:
  copies: list[None] = []
  states = extension_state.ExtensionStates(
    {'plugin': 1}, {'plugin': _Configuration(copies)}
  )
  updated = states.updated('plugin', 2)
  assert copies == [None], 'Only configuration ingress should copy config'
  assert states['plugin'] == 1
  assert updated['plugin'] == 2


def test_updated_configuration_remains_isolated_from_callers() -> None:
  values: config.JsonObject = {'nested': ['original']}
  states = extension_state.ExtensionStates({'plugin': 1}, {'plugin': values})
  updated = states.updated('plugin', 2)
  values['nested'] = ['changed input']
  received = updated.configuration('plugin')
  nested = received['nested']
  assert isinstance(nested, list)
  nested.append('changed output')
  assert states.configuration('plugin') == {'nested': ['original']}
  assert updated.configuration('plugin') == {'nested': ['original']}


def test_configured_and_removed_do_not_mutate_shared_snapshots() -> None:
  states = extension_state.ExtensionStates(
    {'plugin': 1}, {'plugin': {'nested': ['original']}}
  )
  updated = states.updated('plugin', 2)
  configured = updated.configured('plugin', {'nested': ['replacement']})
  removed = updated.removed('plugin')
  assert configured.configuration('plugin') == {'nested': ['replacement']}
  assert 'plugin' not in removed
  assert removed.configuration('plugin') == {}
  assert states.configuration('plugin') == {'nested': ['original']}
  assert updated.configuration('plugin') == {'nested': ['original']}
