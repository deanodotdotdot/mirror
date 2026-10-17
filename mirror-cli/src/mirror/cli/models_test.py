"""A saved alias of the default Beam model still selects the Reflection API."""

from __future__ import annotations

import json
import pathlib

import pytest

from mirror.cli import models


def test_find_model_resolves_unversioned_beam_to_reflection(
  monkeypatch: pytest.MonkeyPatch,
  tmp_path: pathlib.Path,
) -> None:
  config = tmp_path / 'models.json'
  config.write_text(json.dumps([]))
  monkeypatch.setenv('MIRROR_MODELS_CONFIG', str(config))

  option = models.find_model('Beam-501B-A23B')

  assert option.model_id == 'Beam-501B-A23B'
  assert option.api == 'reflection'


@pytest.mark.parametrize(
  'selection',
  [
    {'api': 'anthropic-oauth'},
    {'api': 'anthropic-messages', 'model_api': 'anthropic-oauth'},
    {
      'api': 'anthropic-messages',
      'compaction_model': 'compact',
      'compaction_model_api': 'anthropic-oauth',
    },
    {'api': 'anthropic-oauth', 'compaction_model': 'compact'},
    {'api': 'reflection', 'remote_model_api': 'anthropic-oauth'},
  ],
)
def test_excluded_surface_rejects_every_effective_subscription_provider(
  selection: dict[str, object],
) -> None:
  with pytest.raises(ValueError, match='not supported'):
    models.validate_claude_configuration(selection, supported=False)


@pytest.mark.parametrize(
  'conflict',
  [
    {'api_key': 'SYNTHETIC_KEY_CANARY'},
    {'auth_path': '/synthetic/external/credentials'},
    {'base_url': 'https://synthetic.invalid'},
    {'model_base_url': 'https://synthetic.invalid'},
    {
      'compaction_model': 'compact',
      'compaction_model_base_url': 'https://synthetic.invalid',
    },
  ],
)
def test_subscription_rejects_conflicting_configuration_without_echoing_secrets(
  conflict: dict[str, object], monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.delenv('ANTHROPIC_BASE_URL', raising=False)
  with pytest.raises(ValueError) as raised:
    models.validate_claude_configuration(
      {'api': 'anthropic-oauth', **conflict}, supported=True
    )
  assert 'SYNTHETIC_KEY_CANARY' not in str(raised.value)


def test_explicit_default_does_not_hide_conflicting_anthropic_endpoint(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.setenv('ANTHROPIC_BASE_URL', 'https://synthetic.invalid')
  with pytest.raises(ValueError, match='default Anthropic endpoint'):
    models.validate_connection('anthropic-oauth', 'https://api.anthropic.com')


def test_local_api_main_with_oauth_compactor_keeps_separate_credentials(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.delenv('ANTHROPIC_BASE_URL', raising=False)
  models.validate_claude_configuration(
    {
      'api': 'anthropic-messages',
      'base_url': 'https://synthetic.invalid',
      'api_key': 'synthetic-key',
      'compaction_model': 'compact',
      'compaction_model_api': 'anthropic-oauth',
    },
    supported=True,
  )


def test_same_model_id_persists_distinct_subscription_and_api_billing(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.setenv('MIRROR_MODELS_CONFIG', str(tmp_path / 'models.json'))
  for api in ('anthropic-messages', 'anthropic-oauth', 'anthropic-messages'):
    choice = models.ModelOption('same-model', 'same-model', api=api)
    models.save_last_selection(choice)
    assert models.load_last_selection() == choice
    assert 'account_id' not in (tmp_path / 'last_model.json').read_text()
