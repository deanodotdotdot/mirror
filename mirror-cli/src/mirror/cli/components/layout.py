"""Frame-local measurement for the dock's repeatedly queried columns."""

from collections.abc import Sequence
from typing import override

from prompt_toolkit.application import current
from prompt_toolkit.layout import containers, dimension

from mirror.cli import workspace_application


class Columns(containers.VSplit):
  """Share column measurement within a frame without caching dynamic content."""

  def __init__(self, children: Sequence[containers.AnyContainer]) -> None:
    super().__init__(children)
    self._frame: tuple[object, int] | None = None
    self._heights: dict[tuple[int, int], dimension.Dimension] = {}

  @override
  def preferred_height(
    self, width: int, max_available_height: int
  ) -> dimension.Dimension:
    app = current.get_app()
    if not (
      isinstance(app, workspace_application.WorkspaceApplication)
      and app.rendering
    ):
      self._frame = None
      self._heights.clear()
      return super().preferred_height(width, max_available_height)
    frame = (app, app.render_counter)
    if frame != self._frame:
      self._frame = frame
      self._heights.clear()
    key = (width, max_available_height)
    if key not in self._heights:
      self._heights[key] = super().preferred_height(width, max_available_height)
    return self._heights[key]
