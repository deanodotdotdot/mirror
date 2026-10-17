"""Transient Claude login UI with no chat, history, action or logging payloads."""

from __future__ import annotations

import asyncio
import dataclasses
from collections.abc import Callable
from typing import override

from prompt_toolkit import buffer, data_structures, history, key_binding
from prompt_toolkit.formatted_text import StyleAndTextTuples
from prompt_toolkit.layout import controls, processors

from mirror.cli import claude_auth


@dataclasses.dataclass(frozen=True)
class LoginOutcome:
  """Native grant outcome and the independent UI selection dismissal."""

  result: claude_auth.Completed | claude_auth.Failed
  dismissed: bool


class _LoginControl(controls.BufferControl):
  """Add real display lines above the isolated single-line callback buffer."""

  def __init__(
    self,
    value: buffer.Buffer,
    bindings: key_binding.KeyBindings,
    notice: Callable[[], str],
  ) -> None:
    self._notice = notice
    super().__init__(
      buffer=value,
      include_default_input_processors=False,
      input_processors=[
        processors.PasswordProcessor(''),
        processors.BeforeInput('Hidden input › '),
      ],
      key_bindings=bindings,
    )

  @override
  def create_content(
    self, width: int, height: int, preview_search: bool = False
  ) -> controls.UIContent:
    private = super().create_content(
      width, height, preview_search=preview_search
    )
    notice = self._notice().splitlines()

    def line(index: int) -> StyleAndTextTuples:
      return (
        [('', notice[index])]
        if index < len(notice)
        else private.get_line(index - len(notice))
      )

    cursor = private.cursor_position or data_structures.Point(0, 0)
    return controls.UIContent(
      get_line=line,
      line_count=len(notice) + private.line_count,
      cursor_position=data_structures.Point(cursor.x, cursor.y + len(notice)),
    )


class LoginPrompt:
  """Only this private object holds a URL or callback while login is pending."""

  def __init__(self, invalidate: Callable[[], None], *, offer: bool) -> None:
    self._invalidate = invalidate
    self._notice = (
      'Claude subscription needs a Mirror login.\n'
      'Press Enter to open sign-in, or Escape to keep your selection and draft.'
      if offer
      else 'Preparing Claude subscription login…'
    )
    self._offering = offer
    self._ready = asyncio.Event()
    self._values: asyncio.Queue[str] = asyncio.Queue(maxsize=1)
    self._cancelled = asyncio.Event()
    self.buffer = buffer.Buffer(multiline=False, history=history.DummyHistory())
    bindings = key_binding.KeyBindings()

    @bindings.add('enter')
    def accept(_: object) -> None:
      if self._offering:
        self._offering = False
        self.buffer.reset()
        self._ready.set()
      elif not self._values.full():
        value = self.buffer.text
        self.buffer.reset()
        if len(value.encode('utf-8')) > claude_auth.MAX_CALLBACK:
          self.notice('Callback too long. Paste the final callback URL again.')
        else:
          self._values.put_nowait(value)

    @bindings.add('escape', eager=True)
    @bindings.add('c-c')
    @bindings.add('c-d')
    def cancel(_: object) -> None:
      self.buffer.reset()
      self._cancelled.set()

    self.control = _LoginControl(self.buffer, bindings, lambda: self._notice)

  def notice(self, message: str) -> None:
    # Render directly by prompt_toolkit, never via ShowText or Rich markup.
    self._notice = message.rstrip() + '\nEscape or Ctrl-C cancels.'
    self._invalidate()

  async def _wait[Value](self, work: asyncio.Task[Value]) -> Value:
    cancelled = asyncio.create_task(self._cancelled.wait())
    try:
      await asyncio.wait((work, cancelled), return_when=asyncio.FIRST_COMPLETED)
      if self._cancelled.is_set():
        raise claude_auth.CommandCancelled()
      return work.result()
    finally:
      work.cancel()
      cancelled.cancel()
      await asyncio.gather(work, cancelled, return_exceptions=True)

  async def confirm(self) -> None:
    if self._offering:
      await self._wait(asyncio.create_task(self._ready.wait()))

  async def read_callback(self) -> str:
    return await self._wait(asyncio.create_task(self._values.get()))

  @property
  def dismissed(self) -> bool:
    return self._cancelled.is_set()

  async def cancelled(self) -> None:
    await self._cancelled.wait()

  def clear(self) -> None:
    self.buffer.reset()
    self._notice = ''
    while not self._values.empty():
      self._values.get_nowait()
