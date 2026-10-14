"""View controls and a color-coded mode badge at the right of the status bar."""

from __future__ import annotations

import dataclasses
from collections.abc import Sequence
from typing import override

from prompt_toolkit.layout import containers, controls

from mirror.cli import actions, commands, extension, keymap, state


@dataclasses.dataclass(frozen=True)
class _State:
  pass


def _view(context: commands.CommandContext, args: str) -> actions.Action:
  del context
  token = args.strip().lower()
  if not token:
    return actions.CycleViewMode()
  try:
    mode = actions.ViewMode(token)
  except ValueError:
    modes = ', '.join(mode.value for mode in actions.ViewMode)
    return actions.ShowText(
      f'Unknown view mode: {args}. Modes: {modes}', style='ui.warning'
    )
  return actions.SetViewMode(mode)


class ViewModeExtension(extension.Extension[_State, None, None]):
  """Own the view command, shortcut and status badge for local and remote sessions."""

  @classmethod
  @override
  def id(cls) -> str:
    return 'core/view-mode'

  @staticmethod
  @override
  def initial_state() -> _State:
    return _State()

  @override
  def render_region(self) -> extension.RenderRegion:
    return extension.RenderRegion.STATUS_RIGHT

  @override
  def render_order(self) -> int:
    return 100  # After the throughput and context widgets.

  @override
  def status_separator(self) -> str:
    return ' '

  @override
  def tips(self) -> tuple[str, ...]:
    return ('Use Ctrl+T to cycle through detail, default, and focus views',)

  @override
  def keybindings(self) -> Sequence[keymap.Binding]:
    return (keymap.Binding(('c-t',), actions.CycleViewMode()),)

  @override
  def slash_commands(self) -> Sequence[commands.CommandDef]:
    return (
      commands.CommandDef(
        'view',
        'Cycle view: detail, default (grouped tools + reasoning), focus; Ctrl+T',
        _view,
        id=commands.CommandId.VIEW,
        arguments=commands.OptionalArguments('[mode]'),
        available_when_busy=True,
      ),
    )

  @override
  def render(
    self, global_state: state.State, extension_state: _State
  ) -> containers.AnyContainer | None:
    del extension_state
    if global_state.session.pending_switch is not None:
      return None
    mode = global_state.status_bar.view_mode.value
    return containers.Window(
      controls.FormattedTextControl([
        (f'class:ui.view-{mode}', f' {mode.upper()} ')
      ]),
      dont_extend_height=True,
      dont_extend_width=True,
    )
