import asyncio
import threading

import pytest
from prompt_toolkit import application
from prompt_toolkit.input import defaults
from prompt_toolkit.key_binding import key_bindings, key_processor
from prompt_toolkit.layout import containers, layout
from prompt_toolkit.output import base
from rich import console

from mirror.cli import actions, rendering, transcript, transcript_view


class _BlockedRendering:
  def __init__(self) -> None:
    self.loop = asyncio.get_running_loop()
    self.started = asyncio.Event()
    self.finished = asyncio.Event()
    self.release = threading.Event()
    self.timed_out = False

  def __rich_console__(
    self, output: console.Console, options: console.ConsoleOptions
  ) -> console.RenderResult:
    del output, options
    self.loop.call_soon_threadsafe(self.started.set)
    self.timed_out = not self.release.wait(10)
    yield 'OLD WORKSPACE'
    self.loop.call_soon_threadsafe(self.finished.set)


@pytest.mark.asyncio
@pytest.mark.parametrize('cache_bytes', [0, 8 * 1024 * 1024])
async def test_render_keeps_input_live_and_discards_switched_work(
  cache_bytes: int,
) -> None:
  source = transcript.Transcript(
    view_mode=lambda: actions.ViewMode.DETAIL,
    skill_names=frozenset(),
    get_rewind_entry_id=lambda: None,
  )
  blocked = _BlockedRendering()
  source.add_decoration(blocked)
  view = transcript_view.TranscriptView(
    source.blocks, lambda: (), max_cache_bytes=cache_bytes
  )
  bindings = key_bindings.KeyBindings()
  interrupted, switched = asyncio.Event(), asyncio.Event()
  frames: list[str] = []

  @bindings.add('c-c')
  def interrupt(event: key_processor.KeyPressEvent) -> None:
    del event
    interrupted.set()

  def after_render(app: application.Application[None]) -> None:
    del app
    content = view.create_content(80, 10)
    frame = '\n'.join(
      ''.join(fragment[1] for fragment in content.get_line(index))
      for index in range(content.line_count)
    )
    frames.append(frame)
    if blocked.finished.is_set() and 'NEW WORKSPACE' in frame:
      switched.set()

  with defaults.create_pipe_input() as pipe:
    app: application.Application[None] = application.Application(
      layout=layout.Layout(containers.Window(view)),
      key_bindings=bindings,
      input=pipe,
      output=base.DummyOutput(),
      after_render=after_render,
    )
    running = asyncio.create_task(app.run_async())
    try:
      async with asyncio.timeout(15):
        await blocked.started.wait()
        pipe.send_text('\x03')
        await interrupted.wait()
        assert not blocked.timed_out, 'rendering blocked keyboard dispatch'
        view.clear()
        source.clear_history()
        source.add_decoration(rendering.assistant_render('NEW WORKSPACE'))
        blocked.release.set()
        app.invalidate()
        await switched.wait()
        assert not any('OLD WORKSPACE' in frame for frame in frames)
    finally:
      blocked.release.set()
      app.exit()
      await running
