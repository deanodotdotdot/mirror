"""Optional color-coded view-mode badge at the right of the status bar."""

from __future__ import annotations

import dataclasses
from typing import override

from prompt_toolkit.layout import containers, controls

from mirror.cli import extension, state


@dataclasses.dataclass(frozen=True)
class _State:
  pass


class ViewModeExtension(extension.Extension[_State, None, None]):
  """Show the current view mode when explicitly enabled in /extensions."""

  @classmethod
  @override
  def id(cls) -> str:
    return 'core/view-mode'

  @override
  def default_enabled(self) -> bool:
    return False

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
