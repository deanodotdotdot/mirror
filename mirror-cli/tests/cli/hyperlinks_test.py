import io

import pytest
from prompt_toolkit import data_structures, formatted_text
from prompt_toolkit.output import vt100

from mirror.cli import hyperlinks, rendering, themes

_URL = 'https://example.com/actual?q=one&other=two#section'


@pytest.mark.parametrize('theme_name', themes.NAMES)
def test_markdown_links_preserve_labels_destinations_and_styles(
  theme_name: str,
) -> None:
  output = rendering.renderable_to_ansi(
    rendering.assistant_render(f'[label]({_URL}) after'),
    30,
    theme_name=theme_name,
  )
  fragments = hyperlinks.ANSI(output).__pt_formatted_text__()
  assert (
    formatted_text.fragment_list_to_text(fragments).strip() == '● label after'
  )
  escapes = [
    text for style, text, *_ in fragments if style == '[ZeroWidthEscape]'
  ]
  assert escapes and all(value.endswith(f';{_URL}\x1b\\') for value in escapes)
  assert all(len(fragment) == 2 for fragment in fragments), 'No click handlers'
  link = themes.rich_theme(theme_name).styles['markdown.link_url']
  assert link == themes.rich_theme(theme_name).styles['ui.link']
  assert link.underline and not link.dim
  assert (
    themes
    .prompt_style(theme_name)
    .get_attrs_for_style_str('class:ui.link')
    .underline
  )


def test_native_output_closes_links_before_unlinked_text() -> None:
  buffer = io.StringIO()
  output = hyperlinks.native_output(
    vt100.Vt100_Output(
      buffer, lambda: data_structures.Size(rows=8, columns=30), enable_cpr=False
    )
  )
  opening = f'\x1b]8;;{_URL}\x1b\\'
  output.write_raw(opening)
  output.write('label')
  output.write(' after')
  output.flush()
  assert buffer.getvalue() == f'{opening}label\x1b]8;;\x1b\\ after'


def test_untrusted_controls_and_code_are_not_clickable() -> None:
  source = (
    '\x1b]8;;https://evil.example\x1b\\inert\x1b]8;;\x1b\\'
    '\x01\x1b]52;c;payload\x07\x02\x9b?1049h'
    '\n\n`[inline](https://example.com)`'
    '\n\n```text\n[code](https://example.com)\n```'
  )
  output = rendering.renderable_to_ansi(rendering.assistant_render(source), 80)
  fragments = hyperlinks.ANSI(output).__pt_formatted_text__()
  assert '\x1b]8;' not in output
  assert 'evil.example' not in output
  assert 'payload' not in output
  assert '1049' not in output
  assert '\x1b' not in formatted_text.fragment_list_to_text(fragments)
  assert '[inline](https://example.com)' in rendering.strip_ansi(output)
  assert '[code](https://example.com)' in rendering.strip_ansi(output)
