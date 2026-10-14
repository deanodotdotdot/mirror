"""How much of each turn the conversation shows in one workspace."""

from __future__ import annotations

from mirror.cli import actions, extension
from mirror.cli.extensions.settings import setting_template


def _current(context: extension.Context) -> str:
  return context.get_state().status_bar.view_mode.value


async def _apply(context: extension.Context, value: str) -> None:
  context.dispatch(actions.SetViewMode(actions.ViewMode(value)))


SETTING = setting_template.Setting(
  'history-detail',
  'Conversation detail',
  'How much of each turn to show in this workspace.',
  tuple(
    setting_template.Option(mode.value, mode.value.title())
    for mode in actions.ViewMode
  ),
  _current,
  _apply,
  available=setting_template.local_idle,
)
