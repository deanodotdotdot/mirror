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
