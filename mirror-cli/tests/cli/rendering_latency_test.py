"""Constructing off-screen output must not run the Markdown parser."""

import cProfile
import types

from mirror.cli import rendering


def _parse_calls(profile: cProfile.Profile) -> int:
  return sum(
    entry.callcount
    for entry in profile.getstats()
    if isinstance(entry.code, types.CodeType)
    and entry.code.co_name == 'parse'
    and entry.code.co_filename.endswith('markdown_it/main.py')
  )


def test_assistant_turn_renderable_defers_and_caches_markdown_parsing() -> None:
  with cProfile.Profile() as constructing:
    answer = rendering.assistant_turn_renderable(
      'Reasoning **text**', 'Answer **text**'
    )
  assert answer is not None
  assert _parse_calls(constructing) == 0

  with cProfile.Profile() as painting:
    narrow = rendering.renderable_to_ansi(answer, 40)
    wide = rendering.renderable_to_ansi(answer, 80)
  assert _parse_calls(painting) == 2, 'Parse reasoning and answer once each'
  assert 'Reasoning' in narrow
  assert 'Answer' in wide
