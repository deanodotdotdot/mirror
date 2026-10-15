"""The optional view badge does not own the always-available view controls."""

from __future__ import annotations

import dataclasses

import pytest
from prompt_toolkit.layout import containers, mouse_handlers, screen

from mirror.cli import actions, commands, extension, keymap, state
from mirror.cli.extensions.view_mode import extension as view_mode


@pytest.mark.parametrize('busy', [False, True])
@pytest.mark.parametrize(
  ('argument', 'expected'),
  [
    ('', actions.CycleViewMode()),
    ('default', actions.SetViewMode(actions.ViewMode.DEFAULT)),
    ('focus', actions.SetViewMode(actions.ViewMode.FOCUS)),
    (' DETAIL ', actions.SetViewMode(actions.ViewMode.DETAIL)),
    (
      'compact',
      actions.ShowText(
        'Unknown view mode: compact. Modes: detail, default, focus',
        style='ui.warning',
      ),
    ),
    (
      'unknown',
      actions.ShowText(
        'Unknown view mode: unknown. Modes: detail, default, focus',
        style='ui.warning',
      ),
    ),
  ],
)
def test_view_command(
  busy: bool, argument: str, expected: actions.Action
) -> None:
  context = commands.CommandContext(
    'model', 'medium', 'conversation', busy, '/workspace'
  )
  assert commands.dispatch(f'/view {argument}', context) == expected


@pytest.mark.parametrize(
  ('mode', 'label', 'role'),
  [
    (actions.ViewMode.DEFAULT, 'DEFAULT', 'view-default'),
    (actions.ViewMode.FOCUS, 'FOCUS', 'view-focus'),
    (actions.ViewMode.DETAIL, 'DETAIL', 'view-detail'),
  ],
)
def test_badge_has_only_mode_name_on_colored_background(
  mode: actions.ViewMode,
  label: str,
  role: str,
) -> None:
  plugin = view_mode.ViewModeExtension()
  current = state.State(
    activity=state.ActivityState(),
    status_bar=state.StatusBarState('model', 'medium', 'auto', view_mode=mode),
    session=state.SessionState('conversation'),
    lifecycle=state.LifecycleState(active=True),
  )
  rendered = plugin.render(current, plugin.initial_state())
  assert rendered is not None
  container = containers.to_container(rendered)
  width = container.preferred_width(80).preferred
  painted = screen.Screen()
  container.write_to_screen(
    painted,
    mouse_handlers.MouseHandlers(),
    screen.WritePosition(0, 0, width, 1),
    '',
    True,
    None,
  )
  assert (
    ''.join(painted.data_buffer[0][x].char for x in range(width))
    == f' {label} '
  )
  assert f'class:ui.{role}' in painted.data_buffer[0][1].style
  assert plugin.render_region() is extension.RenderRegion.STATUS_RIGHT
  assert plugin.status_separator() == ' '
  assert plugin.keybindings() == ()
  assert plugin.slash_commands() == ()
  switching = dataclasses.replace(
    current,
    session=dataclasses.replace(
      current.session, pending_switch=actions.PendingSwitch('next', False)
    ),
  )
  assert plugin.render(switching, plugin.initial_state()) is None


def test_core_supplies_view_controls_without_badge() -> None:
  current = state.State(
    activity=state.ActivityState(),
    status_bar=state.StatusBarState('model', 'medium', 'auto'),
    session=state.SessionState('conversation'),
    lifecycle=state.LifecycleState(active=True),
  )
  assert 'view' in {command.name for command in commands.COMMANDS}
  bindings = keymap.defaults(
    current, completion_visible=False, modal_active=False, can_focus_tasks=False
  )
  assert keymap.Binding(('c-t',), actions.CycleViewMode()) in bindings


def test_view_mode_defaults_to_default_view() -> None:
  assert (
    state.StatusBarState('model', 'medium', 'auto').view_mode
    is actions.ViewMode.DEFAULT
  )


@pytest.mark.asyncio
async def test_badge_is_disabled_by_default_and_can_be_enabled() -> None:
  registry = extension.ExtensionRegistry()
  plugin = view_mode.ViewModeExtension()
  assert not await registry.register(plugin, None)
  assert await registry.register(plugin, None, overrides={'enabled': True})
  await registry.close()
