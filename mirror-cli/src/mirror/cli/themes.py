"""Small semantic palette shared by Rich and prompt-toolkit."""

from __future__ import annotations

import dataclasses
import functools
import os
import re
import types
from collections.abc import Mapping, Sequence
from typing import cast, overload, override

from prompt_toolkit import styles as prompt_styles
from prompt_toolkit.output import color_depth
from prompt_toolkit.styles import base
from rich import color, style, theme

from mirror import config
from mirror.cli import extension_state

EXTENSION_ID = 'core/themes'
CONFIG_KEY = 'builtin/themes'
NAMES = ('dark', 'light', 'terminal', 'mono')
DEFAULT_BACKGROUND = 'default'

# Color choices are persisted user data, not selection or health indicators.
TAG_COLORS = (
  ('#d75f5f', 'Red'),
  ('#5f87af', 'Blue'),
  ('#72b58e', 'Green'),
  ('#d7af5f', 'Yellow'),
  ('#af87ff', 'Purple'),
)

# Brand accents, information and mode badges keep distinct visual identities.
# Composites own their foreground/background contrast together.
_DARK = {
  'canvas': '',
  'text': 'default',
  'muted': '#8c8c8c',
  'highlight': '#ffffff',
  'faint': '#717171',
  'accent': '#72b58e',
  # Lift the reference blue enough to read on the dark composer, not just black.
  'info': '#7aa2cc',
  'success': '#3cb371',
  'warning': '#d7af5f',
  'error': '#f28b82',
  'input': '#d3e3da on #282828',
  'message': 'white on #282828',
  'selected': 'bold #ffffff on #1e3207 not reverse',
  'view-default': 'bold #000000 on #5f87af not reverse',
  'view-focus': 'bold #000000 on #87af87 not reverse',
  'view-detail': 'bold #000000 on #af87af not reverse',
  # Muted in truecolor, but still chromatic after 256-color quantization.
  'addition': 'on #063a0c',
  'deletion': 'on #3f090f',
  'code': 'on #262626',
  'code-rail': '#5f87af',
  'heading': '#afafff',
  'quote-rail': '#afafff',
  'inline-code': '#8fbcbb on #2a2a2a',
  'link': 'underline #7aa2cc',
  'tool-heading': '#9e9e9e',
  # Preserve the selector's sage/mint hierarchy on its green inset surface.
  'menu': '#87a995 on #171d1a',
  'menu-accent': '#3f8f68',
  'menu-muted': '#74827a',
  'menu-selected': 'bold #ffffff on #3f8f68 not reverse',
  'menu-selected-detail': 'not bold #b7d7c3 on #3f8f68 not reverse',
  'menu-scroll': '#56645b',
  'menu-disabled': '#383838',
  'welcome-logo': '#72b58e',
  'welcome-axis': '#383838',
  'welcome-reflection': '#585858',
  'welcome-reflection-deep': '#383838',
  'welcome-dots': '#3a3a3a',
}
_LIGHT = {
  'canvas': '',
  'text': 'default',
  'muted': '#53625a',
  'highlight': '#202020',
  'faint': '#69746f',
  'accent': '#1e3207',
  'info': '#355f85',
  'success': '#36652e',
  'warning': '#855000',
  'error': '#a12222',
  'input': '#20382a on #eeeeee',
  'message': '#202020 on #eeeeee',
  'selected': 'bold #ffffff on #1e3207 not reverse',
  'view-default': 'bold #000000 on #5f87af not reverse',
  'view-focus': 'bold #000000 on #87af87 not reverse',
  'view-detail': 'bold #000000 on #af87af not reverse',
  'addition': 'on #d1eed9',
  'deletion': 'on #f8dada',
  'code': 'on #f3f3f3',
  'code-rail': '#355f85',
  'heading': '#625295',
  'quote-rail': '#625295',
  'inline-code': '#345d61 on #eeeeee',
  'link': 'underline #355f85',
  'tool-heading': '#53625a',
  'menu': '#20382a on #e5eee8',
  'menu-accent': '#1e3207',
  'menu-muted': '#53625a',
  'menu-selected': 'bold #ffffff on #1e3207 not reverse',
  'menu-selected-detail': 'not bold #e5eee8 on #1e3207 not reverse',
  'menu-scroll': '#53625a',
  'menu-disabled': '#aab7ac',
  'welcome-logo': '#1e3207',
  'welcome-axis': '#c0c6c2',
  'welcome-reflection': '#8c9690',
  'welcome-reflection-deep': '#c0c6c2',
  'welcome-dots': '#d0d4d1',
}
_TERMINAL = {
  'canvas': '',
  'text': 'default',
  'muted': 'default',
  'highlight': 'bold',
  'faint': 'dim default',
  'accent': 'green',
  'info': 'bright_blue',
  'success': 'green',
  'warning': 'yellow',
  'error': 'bold red',
  'input': 'default',
  'message': 'default',
  'selected': 'bold default on default reverse',
  'view-default': 'bold white on blue not reverse',
  'view-focus': 'bold white on green not reverse',
  'view-detail': 'bold white on magenta not reverse',
  'addition': '',
  'deletion': '',
  'code': '',
  'code-rail': 'blue',
  'heading': 'default',
  'quote-rail': 'dim default',
  'inline-code': 'bold',
  'link': 'underline bright_blue',
  'tool-heading': 'default',
  'menu': 'default',
  'menu-accent': 'green',
  'menu-muted': 'default',
  'menu-selected': 'bold default reverse',
  'menu-selected-detail': 'not bold reverse',
  'menu-scroll': 'default',
  'menu-disabled': 'dim default',
  'welcome-logo': 'green',
  'welcome-axis': 'dim default',
  'welcome-reflection': 'default',
  'welcome-reflection-deep': 'dim default',
  'welcome-dots': 'dim default',
}
_MONO = dict.fromkeys(_DARK, 'default') | {
  'canvas': '',
  'highlight': 'bold',
  'code': '',
  'code-rail': 'dim',
  'quote-rail': 'dim',
  'inline-code': 'bold',
  'link': 'underline',
  'addition': '',
  'deletion': '',
  'faint': 'dim',
  'selected': 'bold default on default reverse',
  'view-default': 'bold default on default reverse',
  'view-focus': 'bold default on default reverse',
  'view-detail': 'bold default on default reverse',
  'error': 'bold',
  'warning': 'bold',
  'accent': 'bold',
  'success': 'bold',
  'menu-accent': 'bold',
  'menu-selected': 'bold default reverse',
  'menu-selected-detail': 'not bold reverse',
  'menu-disabled': 'dim',
  'welcome-logo': 'bold',
  'welcome-axis': 'dim',
  'welcome-reflection-deep': 'dim',
  'welcome-dots': 'dim',
}

# Custom palettes need not repeat component colors when overriding shared roles.
_FALLBACK_ROLES = {
  'highlight': 'text',
  'message': 'input',
  'menu': 'input',
  'menu-accent': 'accent',
  'menu-muted': 'muted',
  'menu-selected': 'selected',
  'menu-scroll': 'faint',
  'menu-disabled': 'faint',
  'tool-heading': 'muted',
  'code-rail': 'info',
  'quote-rail': 'heading',
  'welcome-logo': 'accent',
  'welcome-reflection-deep': 'welcome-axis',
}
_SURFACE_ROLES = {
  'canvas',
  'input',
  'message',
  'selected',
  'menu',
  'menu-selected',
  'menu-selected-detail',
  'view-default',
  'view-focus',
  'view-detail',
  'addition',
  'deletion',
  'code',
  'inline-code',
}

# Component classes describe placement; their values only choose semantic roles.
_CLASSES = {
  '': 'text',
  'selected': 'selected',
  'activity': 'muted',
  'code-action': 'muted',
  'code-action-hover': 'info',
  'code-hint': 'muted',
  'code-copied': 'success',
  'code-copy-error': 'warning',
  'activity-spinner': 'accent',
  'activity-spinner-highlight': 'text',
  'footer': 'muted',
  'footer-model': 'text',
  'footer-approval-auto': 'success',
  'footer-approval-manual': 'warning',
  'composer': 'input',
  'composer-bottom-divider': 'input-edge',
  'composer footer': 'muted',
  'composer footer-model': 'input',
  'composer footer-divider': 'faint',
  'composer footer-approval-auto': 'success',
  'composer footer-approval-manual': 'warning',
  'composer skill-mention-input': 'info',
  'worktree-tab': 'input',
  'worktree-tab-selected': 'selected',
  'worktree-tab-marker': 'accent',
  'worktree-hint': 'muted.italic',
  'worktree': 'muted',
  'worktree-running': 'accent',
  'worktree-attention': 'warning.bold',
  'worktree-failed': 'error.bold',
  # Reverse-video selection turns status foregrounds into mismatched fills.
  'worktree-tab-selected worktree': 'selected',
  'worktree-tab-selected worktree-running': 'selected',
  'worktree-tab-selected worktree-attention': 'selected',
  'worktree-tab-selected worktree-failed': 'selected',
  'worktree-tab-selected activity-spinner-highlight': 'selected',
  'task': 'muted',
  'task-selected': 'selected',
  'task-header': 'muted.bold',
  'task-tree': 'faint',
  'slash-input-prefix': 'accent.bold',
  'slash-input-command': 'accent',
  'slash-border': 'accent',
  'slash-marker': 'faint',
  'slash-marker-selected': 'accent.bold',
  'slash-command': 'accent',
  'slash-command-selected': 'accent.bold',
  'slash-description': 'muted',
  'slash-description-selected': 'text',
  'slash-locked': 'faint',
  'slash-locked-selected': 'faint.bold',
  'slash-scroll-arrow': 'faint',
  'slash-autocomplete': 'muted',
  'slash-preview-action': 'faint',
  'slash-preview-args': 'muted',
  'slash-preview-locked': 'faint',
  'slash-argument-placeholder': 'faint',
  'skill-border': 'info',
  'skill-scroll-arrow': 'faint',
  'skill-marker': 'faint',
  'skill-marker-selected': 'info.bold',
  'skill-name': 'info',
  'skill-name-selected': 'info.bold',
  'skill-description': 'muted',
  'skill-description-selected': 'text',
  'skill-autocomplete': 'muted',
  'skill-preview-action': 'faint',
  'skill-mention-input': 'info',
  'menu-surface': 'menu',
  'menu-top-padding': 'menu-edge',
  'menu-border': 'menu-accent.bold',
  'menu-section': 'menu-muted.bold',
  'menu-item': 'menu-text',
  'menu-selected': 'menu-selected',
  'menu-detail': 'menu-muted',
  'menu-detail-selected': 'menu-selected-detail',
  'extension-on': 'accent.bold',
  'extension-off': 'muted',
  'menu-selected extension-on': 'menu-selected',
  'menu-selected extension-off': 'menu-selected',
  'menu-selected worktree': 'menu-selected',
  'menu-selected worktree-running': 'menu-selected',
  'menu-selected worktree-attention': 'menu-selected',
  'menu-selected worktree-failed': 'menu-selected',
  'menu-selected activity-spinner-highlight': 'menu-selected',
  'welcome-dots': 'welcome-dots',
  'menu-hint': 'menu-muted.italic',
  'menu-footer': 'menu-muted.italic',
  'menu-scroll': 'menu-scroll',
  'menu-scroll-disabled': 'menu-disabled',
  'resume-scope': 'muted',
  'resume-scope-active': 'text.bold',
  'rewind-banner': 'warning.italic',
}


@dataclasses.dataclass(frozen=True)
class Theme:
  """A named palette of semantic Rich style overrides over a built-in base."""

  id: str
  label: str
  styles: Mapping[str, str] = dataclasses.field(default_factory=dict[str, str])
  base: str = 'dark'

  def __post_init__(self) -> None:
    object.__setattr__(
      self, 'styles', types.MappingProxyType(dict(self.styles))
    )

  @override
  def __hash__(self) -> int:
    return hash((
      self.id,
      self.label,
      self.base,
      tuple(sorted(self.styles.items())),
    ))


def contributions(owner: str, values: object) -> tuple[Theme, ...]:
  """Validate and namespace a provider's entire contribution at registration."""
  if not isinstance(values, Sequence) or isinstance(values, (str, bytes)):
    raise ValueError('Extension themes must be a sequence of Theme values')
  if values and (':' in owner or not owner.isprintable()):
    raise ValueError(
      f'Theme provider ID must be printable without a colon: {owner!r}'
    )
  loaded: dict[str, Theme] = {}
  for value in cast(Sequence[object], values):
    if not isinstance(value, Theme):
      raise ValueError('Extension themes must contain only Theme values')
    if not re.fullmatch(r'[a-z0-9][a-z0-9_-]*', value.id):
      raise ValueError(f'Invalid local theme ID: {value.id!r}')
    if value.id in loaded:
      raise ValueError(f'Duplicate theme ID: {value.id!r}')
    if not value.label.strip() or not value.label.isprintable():
      raise ValueError('Theme labels must be non-empty printable single lines')
    if value.base not in NAMES:
      raise ValueError(f'Unknown theme base: {value.base!r}')
    for role, definition in value.styles.items():
      if role not in _DARK:
        raise ValueError(f'Unknown theme role: {role!r}')
      parsed = style.Style.parse(definition)
      if role not in _SURFACE_ROLES and (parsed.bgcolor or parsed.reverse):
        raise ValueError(
          f'Theme role {role!r} is foreground-only; put backgrounds on '
          "'canvas', 'input', 'menu', or 'selected' instead"
        )
      if role in {'canvas', 'code', 'addition', 'deletion'} and (
        parsed.color or parsed.reverse
      ):
        raise ValueError(f'Theme role {role!r} must contain only a background')
    registered = dataclasses.replace(value, id=f'{owner}:{value.id}')
    # Exercise both renderers inside the extension load fault boundary.
    prompt_style(registered)
    loaded[value.id] = registered
  return tuple(loaded.values())


def configured(
  settings: Mapping[str, object], available: Sequence[Theme] = ()
) -> str:
  """Unknown settings fall back without importing code or changing consent."""
  value = settings.get('name', 'dark')
  names = (*NAMES, *(item.id for item in available))
  return value if isinstance(value, str) and value in names else 'dark'


def saved(settings: Mapping[str, object]) -> str:
  """Resolve the same built-in preference for views without an extension host."""
  try:
    overrides = config.extension_overrides(
      settings.get('extensions'), CONFIG_KEY
    )
    resolved = config.resolve_extension(overrides)
  except ValueError:
    return effective('dark')
  return effective(
    configured(resolved.config) if resolved.enabled is True else 'dark'
  )


@overload
def effective(
  name: str, environment: Mapping[str, str] | None = None
) -> str: ...


@overload
def effective(
  name: Theme, environment: Mapping[str, str] | None = None
) -> str | Theme: ...


def effective(
  name: str | Theme, environment: Mapping[str, str] | None = None
) -> str | Theme:
  """Respect explicit terminal capabilities without querying its background."""
  environment = os.environ if environment is None else environment
  return (
    'mono'
    if 'NO_COLOR' in environment or environment.get('TERM') == 'dumb'
    else name
  )


def prompt_color_depth() -> color_depth.ColorDepth | None:
  """Honor color overrides before adding COLORTERM's truecolor capability."""
  if effective('dark') == 'mono':
    return color_depth.ColorDepth.DEPTH_1_BIT
  if explicit := color_depth.ColorDepth.from_env():
    return explicit
  # prompt-toolkit's VT100 output otherwise quantizes RGB to 256 colors.
  if os.environ.get('COLORTERM', '').lower() in {'truecolor', '24bit'}:
    return color_depth.ColorDepth.DEPTH_24_BIT
  return None


@overload
def current(extensions: extension_state.ExtensionStates) -> str: ...


@overload
def current(
  extensions: extension_state.ExtensionStates, available: Sequence[Theme]
) -> str | Theme: ...


def current(
  extensions: extension_state.ExtensionStates, available: Sequence[Theme] = ()
) -> str | Theme:
  name = configured(extensions.configuration(EXTENSION_ID), available)
  return effective(next((item for item in available if item.id == name), name))


@functools.lru_cache(maxsize=128)
def rich_theme(name: str | Theme = 'dark') -> theme.Theme:
  """Build a read-only-in-use framework theme, never mutate global styles."""
  values = {
    'dark': _DARK,
    'light': _LIGHT,
    'terminal': _TERMINAL,
    'mono': _MONO,
  }[name.base if isinstance(name, Theme) else name]
  if isinstance(name, Theme):
    values = values | dict(name.styles)
    for role, source in _FALLBACK_ROLES.items():
      if source in name.styles and role not in name.styles:
        values[role] = values[source]
    if (
      'selected' in name.styles or 'menu-selected' in name.styles
    ) and 'menu-selected-detail' not in name.styles:
      values['menu-selected-detail'] = values['menu-selected'] + ' not bold'
  # Preserve the published style name without an independent palette color.
  values = values | {'extension-active': values['accent']}
  resolved: dict[str, style.Style] = {}
  for role, value in values.items():
    base = style.Style.parse(value)
    resolved[f'ui.{role}'] = base
    resolved[f'ui.{role}.bold'] = base + style.Style(bold=True)
    resolved[f'ui.{role}.italic'] = base + style.Style(italic=True)
  # Menu labels inherit their container's surface, including unfilled views.
  resolved['ui.menu-text'] = style.Style(color=resolved['ui.menu'].color)
  resolved['ui.input-edge'] = style.Style(
    color=resolved['ui.input'].bgcolor or color.Color.default(),
    bgcolor=resolved['ui.canvas'].bgcolor,
  )
  resolved['ui.menu-edge'] = style.Style(
    color=resolved['ui.menu'].bgcolor or color.Color.default(),
    bgcolor=resolved['ui.canvas'].bgcolor or color.Color.default(),
  )
  resolved.update({
    'markdown.code': resolved['ui.inline-code'],
    'markdown.code_block': style.Style(),
    'markdown.link': resolved['ui.link'],
    'markdown.link_url': resolved['ui.link'],
    'markdown.hr': resolved['ui.faint'],
    'markdown.block_quote': style.Style(italic=True),
    'markdown.table.border': resolved['ui.muted'],
    'markdown.table.header': resolved['ui.text.bold'],
    'markdown.list': resolved['ui.text'],
    'markdown.item.bullet': resolved['ui.muted'],
    'markdown.item.number': resolved['ui.muted'],
    'markdown.kbd': resolved['ui.info.bold'],
    **{
      f'markdown.h{level}': resolved['ui.heading'] + style.Style.parse(emphasis)
      for level, emphasis in enumerate(
        (
          'bold underline',
          'bold',
          'bold italic',
          'italic',
          'italic dim',
          'dim',
        ),
        start=1,
      )
    },
  })
  return theme.Theme(resolved)


def _prompt_color(value: color.Color) -> str:
  if value.type is color.ColorType.STANDARD:
    return base.ANSI_COLOR_NAMES[(value.number or 0) + 1]
  return value.name


def _prompt_style(value: style.Style) -> str:
  parts: list[str] = []
  if value.color is not None:
    parts.append(f'fg:{_prompt_color(value.color)}')
  if value.bgcolor is not None:
    parts.append(f'bg:{_prompt_color(value.bgcolor)}')
  if value.bold is not None:
    parts.append('bold' if value.bold else 'nobold')
  if value.italic:
    parts.append('italic')
  if value.underline:
    parts.append('underline')
  if value.reverse is not None:
    parts.append('reverse' if value.reverse else 'noreverse')
  if value.dim:
    parts.append('dim')
  return ' '.join(parts)


@functools.lru_cache(maxsize=128)
def prompt_style(name: str | Theme = 'dark') -> prompt_styles.Style:
  """Resolve semantic roles once per palette for the live application."""
  rich_styles = rich_theme(name).styles
  definitions = {
    key: _prompt_style(rich_styles[f'ui.{role}'])
    for key, role in _CLASSES.items()
  }
  definitions.update({
    key: _prompt_style(value)
    for key, value in rich_styles.items()
    if key.startswith('ui.')
  })
  # Defaults apply once; a canvas class inherited through scroll panes can
  # otherwise overwrite nested surfaces on their unoccupied cells.
  definitions[''] = _prompt_style(
    rich_styles['ui.text'] + rich_styles['ui.canvas']
  )
  muted = rich_styles['ui.muted'].color
  highlight = rich_styles['ui.highlight'].color
  for level in range(128):
    if muted is not None and highlight is not None:
      channels = zip(muted.get_truecolor(), highlight.get_truecolor())
      shade = ''.join(
        f'{round(start + (end - start) * level / 127):02x}'
        for start, end in channels
      )
      definitions[f'ui.highlight-{level}'] = f'fg:#{shade}'
    else:
      definitions[f'ui.highlight-{level}'] = (
        _prompt_style(rich_styles['ui.highlight']) if level >= 64 else ''
      )
  return prompt_styles.Style.from_dict(definitions)
