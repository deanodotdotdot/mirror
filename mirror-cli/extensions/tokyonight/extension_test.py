import pathlib

import pyte
import pytest
from rich import text

from mirror.cli import extension, rendering, themes
from mirror.cli.extensions.tokyonight import extension as tokyonight


@pytest.mark.asyncio
async def test_tokyonight_is_enabled_by_default_and_can_be_disabled() -> None:
  registry = extension.ExtensionRegistry()
  directory = pathlib.Path(tokyonight.__file__).parent
  try:
    assert await registry.load(directory) == 'tokyonight'
    assert registry.extension_ids() == ('tokyonight',)
    await registry.unload('tokyonight')
    assert await registry.load(directory, overrides={'enabled': False}) is None
    assert registry.extension_ids() == ()
  finally:
    await registry.close()


@pytest.mark.asyncio
async def test_tokyonight_can_be_copied_as_a_user_extension(
  tmp_path: pathlib.Path,
) -> None:
  directory = tmp_path / 'tokyonight'
  directory.mkdir()
  (directory / 'extension.py').write_text(
    pathlib.Path(tokyonight.__file__).read_text()
  )
  registry = extension.ExtensionRegistry()
  try:
    assert (
      await registry.load(directory, overrides={'enabled': True})
      == 'tokyonight'
    )
  finally:
    await registry.close()


@pytest.mark.parametrize(
  ('index', 'theme_id', 'background', 'surface'),
  [(0, 'night', '1a1b26', '16161e'), (1, 'storm', '24283b', '1f2335')],
)
def test_tokyonight_themes_render_in_both_frameworks(
  index: int, theme_id: str, background: str, surface: str
) -> None:
  provider = tokyonight.TokyoNight()
  contributed = themes.contributions(provider.id(), provider.themes())
  assert tuple(theme.id for theme in contributed) == (
    'tokyonight:night',
    'tokyonight:storm',
  )
  palette = contributed[index]
  assert palette.id == f'tokyonight:{theme_id}'
  sheet = themes.prompt_style(palette)
  assert sheet.get_attrs_for_style_str('').bgcolor == background
  assert sheet.get_attrs_for_style_str('class:composer').bgcolor == surface
  assert sheet.get_attrs_for_style_str('class:menu-surface').bgcolor == surface
  assert sheet.get_attrs_for_style_str('class:menu-selected').bold
  terminal = pyte.Screen(20, 2)
  pyte.Stream(terminal).feed(
    rendering.renderable_to_ansi(
      text.Text('Tokyo Night', style='ui.accent'), 20, theme_name=palette
    )
  )
  assert terminal.display[0].strip() == 'Tokyo Night'
  assert terminal.buffer[0][0].fg == '7aa2f7'
