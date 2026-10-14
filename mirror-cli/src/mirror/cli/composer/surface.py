"""Background and padding around the composer input."""

from collections.abc import Callable, Sequence

from prompt_toolkit import buffer, filters
from prompt_toolkit.layout import containers, controls, dimension, processors

from mirror.cli import rendering, themes
from mirror.cli.components import layout
from mirror.cli.composer import processors as composer_processors


def input_window(
  draft: buffer.Buffer,
  *,
  hidden: Callable[[], bool] = lambda: False,
  max_height: Callable[[], int | None] = lambda: None,
  input_processors: Sequence[processors.Processor] = (),
) -> containers.Window:
  """Build the shared editor; callers own its buffer and available actions."""
  return containers.Window(
    controls.BufferControl(
      buffer=draft,
      include_default_input_processors=False,
      input_processors=[
        processors.HighlightSelectionProcessor(),
        composer_processors.SlashInputProcessor(),
        *input_processors,
        processors.BeforeInput(f'{rendering.PROMPT} '),
      ],
    ),
    height=lambda: (
      dimension.Dimension.exact(0)
      if hidden()
      else dimension.Dimension(min=1, max=max_height())
    ),
    dont_extend_height=True,
    wrap_lines=True,
  )


def wrap_input(
  input_window: containers.Window,
  menu_visible: Callable[[], bool],
  status_bar: containers.AnyContainer,
  *,
  theme_name: Callable[[], str | themes.Theme] = lambda: 'dark',
) -> containers.HSplit:
  """Frame the input and status bar without duplicating an open menu border."""
  show_menu = filters.Condition(menu_visible)
  input_surface = containers.HSplit(
    [
      containers.ConditionalContainer(
        containers.Window(height=1), filter=~show_menu
      ),
      containers.ConditionalContainer(
        layout.Columns([
          containers.Window(width=1),
          input_window,
          containers.Window(width=1),
        ]),
        # VSplit's padding otherwise expands when its input has zero height.
        filter=filters.Condition(
          lambda: input_window.preferred_height(1, 1).max > 0
        ),
      ),
      containers.Window(height=1),
      status_bar,
    ],
    style='class:composer',
  )
  return containers.HSplit([
    input_surface,
    containers.Window(
      height=1,
      # Half-blocks only soften an explicit fill, not the terminal background.
      char=lambda: (
        '▀'
        if themes.rich_theme(theme_name()).styles['ui.input'].bgcolor
        else ' '
      ),
      style='class:composer-bottom-divider',
    ),
  ])
