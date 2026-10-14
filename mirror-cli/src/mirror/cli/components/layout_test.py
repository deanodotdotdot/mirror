"""Dock measurements are shared within a draw, never between mutable UI states."""

from typing import override

import pytest
from prompt_toolkit import application
from prompt_toolkit.input import base as input_base
from prompt_toolkit.input import defaults
from prompt_toolkit.layout import containers, dimension
from prompt_toolkit.output import base as output_base

from mirror.cli import workspace_application
from mirror.cli.components import layout


class MeasuringWindow(containers.Window):
  def __init__(self) -> None:
    super().__init__()
    self.measurements = 0
    self.rows = 1

  @override
  def preferred_height(
    self, width: int, max_available_height: int
  ) -> dimension.Dimension:
    del width, max_available_height
    self.measurements += 1
    return dimension.Dimension.exact(self.rows)


def _app() -> workspace_application.WorkspaceApplication:
  app = workspace_application.WorkspaceApplication(
    input=input_base.DummyInput(), output=output_base.DummyOutput()
  )
  app.rendering, app.render_counter = True, 1
  return app


@pytest.mark.parametrize(
  ('width', 'height', 'frame'), [(40, 24, 1), (80, 12, 1), (80, 24, 2)]
)
def test_columns_cache_keys(width: int, height: int, frame: int) -> None:
  child = MeasuringWindow()
  columns = layout.Columns([child])
  app = _app()
  with application.set_app(app):
    assert columns.preferred_height(80, 24).preferred == 1
    columns.preferred_height(80, 24)
    assert child.measurements == 1
    child.rows, app.render_counter = 2, frame
    assert columns.preferred_height(width, height).preferred == 2
    assert child.measurements == 2
  with application.set_app(_app()):
    child.rows = 3
    assert columns.preferred_height(80, 24).preferred == 3


@pytest.mark.asyncio
async def test_columns_only_cache_during_a_real_draw() -> None:
  child = MeasuringWindow()
  columns = layout.Columns([child])

  def finish_frame(app: application.Application[str | None]) -> None:
    assert app.is_running
    assert columns.preferred_height(80, 24).preferred == 1
    columns.preferred_height(80, 24)
    assert child.measurements == 1
    app.after_render -= finish_frame
    app.exit()

  with defaults.create_pipe_input() as pipe:
    app = workspace_application.WorkspaceApplication(
      input=pipe, output=output_base.DummyOutput(), after_render=finish_frame
    )
    await app.run_async()
  assert not app.rendering
  with application.set_app(app):
    child.rows = 2
    assert columns.preferred_height(80, 24).preferred == 2
    child.rows = 3
    assert columns.preferred_height(80, 24).preferred == 3
  child.rows = 4
  assert columns.preferred_height(80, 24).preferred == 4
