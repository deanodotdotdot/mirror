"""Tokyo Night and Storm palettes, available in /themes by default."""

from typing import override

from mirror.cli import extension, themes


# Colors adapted from https://github.com/folke/tokyonight.nvim (Apache-2.0).
def _styles(background: str, surface: str) -> dict[str, str]:
  return {
    # Only canvas/surface roles own backgrounds; text inherits its container.
    'canvas': f'on {background}',
    'text': '#c0caf5',
    'muted': '#a9b1d6',
    'faint': '#737aa2',
    'accent': '#7aa2f7',
    'info': '#7dcfff',
    'success': '#9ece6a',
    'warning': '#e0af68',
    'error': '#f7768e',
    # Menus and user messages inherit input; menu selection inherits selected.
    'input': f'#c0caf5 on {surface}',
    'selected': 'bold #c0caf5 on #283457 not reverse',
    'view-default': 'bold #1a1b26 on #7aa2f7 not reverse',
    'view-focus': 'bold #1a1b26 on #9ece6a not reverse',
    'view-detail': 'bold #1a1b26 on #bb9af7 not reverse',
    # Override `addition`/`deletion` for diff fills; the base supplies them here.
    'code': f'on {surface}',
    'heading': '#bb9af7',
    'inline-code': f'#73daca on {surface}',
    'link': 'underline #7dcfff',
    'welcome-axis': '#3b4261',
    'welcome-reflection': '#565f89',
    'welcome-dots': '#292e42',
  }


class TokyoNight(extension.Extension[None, None, None]):
  @classmethod
  @override
  def id(cls) -> str:
    return 'tokyonight'

  @override
  def themes(self) -> list[themes.Theme]:
    # IDs become tokyonight:night and tokyonight:storm; labels may change freely.
    return [
      themes.Theme(
        'night', 'Tokyo Night', _styles('#1a1b26', '#16161e'), base='dark'
      ),
      themes.Theme(
        'storm', 'Tokyo Night Storm', _styles('#24283b', '#1f2335'), base='dark'
      ),
    ]
