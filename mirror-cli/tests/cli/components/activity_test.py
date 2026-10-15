import pytest
from freezegun import api as freezegun_api
from prompt_toolkit.layout import containers, mouse_handlers, screen

from mirror.cli import state
from mirror.cli.components import activity


@pytest.mark.parametrize('width', [1, 2, 10, 80])
@pytest.mark.parametrize(
  'elapsed, pose',
  [
    (0.05, '*+'),
    (0.15, '+*'),
    (0.25, ':*'),
    (0.35, '.+'),
    (0.45, '+.'),
    (0.55, '*:'),
  ],
)
def test_render_preserves_both_spinner_cells(
  width: int,
  elapsed: float,
  pose: str,
  frozen_ui_time: freezegun_api.FrozenDateTimeFactory,
) -> None:
  frozen_ui_time.move_to('1970-01-01')
  frozen_ui_time.tick(elapsed)
  snapshot = state.State(
    activity=state.ActivityState(busy=True, status='Working'),
    status_bar=state.StatusBarState('model', 'medium', 'auto'),
    session=state.SessionState('test'),
    lifecycle=state.LifecycleState(active=True),
  )
  window = containers.to_container(activity.Activity().render(snapshot))
  target = screen.Screen()
  window.write_to_screen(
    target,
    mouse_handlers.MouseHandlers(),
    screen.WritePosition(0, 0, width, 1),
    '',
    True,
    None,
  )
  row = ''.join(target.data_buffer[0][column].char for column in range(width))
  assert row == f'{pose} Working'[:width].ljust(width)
