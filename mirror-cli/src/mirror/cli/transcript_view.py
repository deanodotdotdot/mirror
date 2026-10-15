"""An anchored, lazily rendered conversation inside the terminal screen."""

from __future__ import annotations

import asyncio
import collections
import contextlib
import dataclasses
import functools
import io
import itertools
import re
import sys
import time
from collections.abc import Callable, Generator, Sequence
from typing import override

from prompt_toolkit import formatted_text, mouse_events, utils
from prompt_toolkit.application import current
from prompt_toolkit.layout import controls
from rich import cells, color, console, styled, text

from mirror.cli import (
  actions,
  hyperlinks,
  rendering,
  screen_selection,
  themes,
  transcript,
  workspace_application,
)


def _summarize(content: str, width: int) -> str:
  """Normalize and clip a prompt without splitting terminal graphemes."""
  content = ' '.join(content.split())
  summary = rendering.icon_prefix(rendering.PROMPT) + rendering.strip_ansi(
    content
  )
  used = end = 0
  for start, stop, size in cells.split_graphemes(summary)[0]:
    # Fit both renderers: their cell widths differ for some emoji sequences.
    used += max(size, utils.get_cwidth(summary[start:stop]))
    if used <= width - 1:
      end = stop
    if used > width:
      return summary[:end].rstrip() + '…'
  return summary


@dataclasses.dataclass(frozen=True)
class Position:
  block: str
  row: int
  part: int = 0


type _Row = tuple[formatted_text.OneStyleAndTextTuple, ...]
type _Rows = tuple[_Row, ...]
type _LayoutKey = tuple[str, tuple[int, int], int, int, str | themes.Theme, int]


def _plain(fragments: _Row) -> str:
  return (
    formatted_text
    .fragment_list_to_text(list(fragments))
    .strip()
    .removeprefix(rendering.icon_prefix(rendering.ASSISTANT_ICON))
  )


def _layout_bytes(key: _LayoutKey, rows: _Rows) -> int:
  """Account for retained Python objects, not just their character counts."""
  size = sys.getsizeof(key) + sys.getsizeof(rows) + sys.getsizeof((rows, 0))
  size += sum(map(sys.getsizeof, key)) + sum(map(sys.getsizeof, key[1]))
  size += sys.getsizeof(0)
  strings: set[int] = set()
  for row in rows:
    size += sys.getsizeof(row)
    for fragment in row:
      size += sys.getsizeof(fragment)
      for value in fragment:
        if id(value) not in strings:
          strings.add(id(value))
          size += sys.getsizeof(value)
          if isinstance(value, functools.partial):
            size += sys.getsizeof(value.args) + sys.getsizeof(value.keywords)
            size += sys.getsizeof(value.func)
            for argument in value.args:
              if isinstance(argument, str) and id(argument) not in strings:
                strings.add(id(argument))
                size += sys.getsizeof(argument)
  return size


def _split_rows(
  fragments: formatted_text.StyleAndTextTuples, padding: int
) -> _Rows:
  """Coalesce spans without dropping their actions or retaining terminal fill."""
  while fragments and fragments[-1][0] == '':
    last = fragments.pop()
    if text := last[1].rstrip():
      fragments.append(('', text))
      break
  # ANSI decoding emits one fragment per character; split style runs instead.
  runs: formatted_text.StyleAndTextTuples = []
  for _, group in itertools.groupby(
    fragments, key=lambda fragment: (fragment[0], fragment[2:])
  ):
    parts = list(group)
    first = parts[0]
    text = ''.join(part[1] for part in parts)
    runs.append(
      (first[0], text, first[2]) if len(first) == 3 else (first[0], text)
    )
  rows = [tuple(line) for line in formatted_text.split_lines(runs)]
  while rows and not any(text for _, text, *_ in rows[-1]):
    rows.pop()
  return tuple(rows) + ((),) * padding


class TranscriptView(screen_selection.HoverControl):
  """Paint visible rows with bounded retained layouts and stable reading positions."""

  def __init__(
    self,
    source: Callable[[], tuple[transcript.Block, ...]],
    live: Callable[[], Sequence[console.RenderableType]],
    *,
    navigate: Callable[[actions.NavigateHistory], object] | None = None,
    pending: Callable[[], Sequence[transcript.Block]] = lambda: (),
    theme_name: Callable[[], str | themes.Theme] = lambda: 'dark',
    max_cache_bytes: int = 8 * 1024 * 1024,
  ) -> None:
    if max_cache_bytes < 0:
      raise ValueError('max_cache_bytes must be nonnegative')
    self._max_cache_bytes = max_cache_bytes
    self._source = source
    self._pending = pending
    self._navigate = navigate or self.navigate
    self._live = live
    self._theme_name = theme_name
    self._anchor: Position | None = None
    self._anchor_index = 0
    self._width = 0
    self._height = 0
    self._blocks: Sequence[transcript.Block] = ()
    self._settled_snapshot: tuple[transcript.Block, ...] = ()
    self._pending_snapshot: tuple[transcript.Block, ...] = ()
    self._live_source: Sequence[console.RenderableType] = ()
    self._live_snapshot: tuple[transcript.Block, ...] = ()
    self._live_revision = 0
    self._settling_line: tuple[str, int, int, bool] | None = None
    self._cache: collections.OrderedDict[_LayoutKey, tuple[_Rows, int]] = (
      collections.OrderedDict()
    )
    self._copy_ids = itertools.count()
    self._hovered_style: str | None = None
    self._hovered_action = False
    self._copied_style: str | None = None
    self._copy_label = 'Copied'
    self._copy_until = 0.0
    self._visible: _Rows = ()
    self._cache_bytes = 0
    self._render_task: asyncio.Task[None] | None = None
    self._render_needed: tuple[transcript.Block, _LayoutKey] | None = None
    self._layout_ready = True
    self._generation = 0
    # The cache budget excludes transient rendering and the visible row slice.
    # Keep both sides of a boundary, not every layout crossed by a large jump.
    self._oversized: collections.deque[tuple[_LayoutKey, _Rows]] = (
      collections.deque(maxlen=2)
    )
    self._header_height = 0
    self._user_position: tuple[int, int, int | None] | None = None
    self._summary: tuple[str, int, str] | None = None

  def _remember_line(
    self, index: int, part: int, row: int, first: int | None = None
  ) -> None:
    """Locate reading content across rewrapping and preview settlement."""
    rows = self._rows(index, part)
    target_row = next(
      (i for i in range(row, len(rows)) if _plain(rows[i])), row
    )
    pattern = r'\s+'.join(
      re.escape(word) for word in _plain(rows[target_row]).split()
    )
    if not pattern:
      return
    preceding: list[str] = []
    if first is not None:
      for block_index in range(first, index + 1):
        stop = (
          part if block_index == index else len(self._blocks[block_index].parts)
        )
        for part_index in range(stop):
          preceding.extend(map(_plain, self._rows(block_index, part_index)))
    preceding.extend(map(_plain, rows[:target_row]))
    occurrence = (
      sum(1 for _ in re.finditer(pattern, '\n'.join(preceding)))
      if pattern
      else 0
    )
    self._settling_line = (
      pattern,
      occurrence,
      target_row - row,
      first is not None,
    )

  def settle_live(self, identities: str | Sequence[str]) -> None:
    """Transfer the preview anchor without laying out the completed answer."""
    if self._anchor is None or not self._anchor.block.startswith('live:'):
      return
    with self._layout_pass():
      index, part, row = self._start()
      if not isinstance(identities, str):
        live_index = int(self._anchor.block.removeprefix('live:'))
        if live_index < len(identities):
          self._anchor = Position(identities[live_index], row, part)
        return
      first = len(self._settled_snapshot) + len(self._pending_snapshot)
      current_part = self._blocks[index].parts[part]
      final = next(
        (
          block for block in reversed(self._source()) if block.key == identities
        ),
        None,
      )
      # Unmapped answer roots cannot share an unambiguous source position.
      roots = sum(
        isinstance(block.renderable, rendering.Answer)
        and not block.renderable.continuation
        and block.renderable.source_offset == 0
        for block in self._live_snapshot
      )
      if (
        isinstance(current_part, rendering.Answer)
        and final is not None
        and roots == 1
      ):
        final_part = next(
          (
            i
            for i, value in enumerate(final.parts)
            if isinstance(value, rendering.Answer)
            and value.source_offset == current_part.source_offset
            and value.source.startswith(current_part.source)
          ),
          None,
        )
        if final_part is not None:
          self._remember_line(index, part, row)
          self._anchor = Position(identities, row, final_part)
          return
      self._remember_line(index, part, row, first)
      self._anchor = Position(identities, row)

  def follow(self) -> None:
    """Resume following the latest conversation output."""
    self._anchor = None
    self._settling_line = None

  def first(self) -> None:
    """Reach the oldest entry without formatting the intervening history."""
    if self._blocks:
      self._anchor_index = 0
      self._settling_line = None
      self._anchor = Position(self._blocks[0].key, 0)

  @property
  def reading(self) -> bool:
    """Whether incoming output should leave the reading anchor in place."""
    return self._anchor is not None

  def clear(self) -> None:
    """Release layouts when their conversation changes."""
    self.clear_hover()
    self._copied_style = None
    self._visible = ()
    self._anchor = None
    self._anchor_index = 0
    self._settling_line = None
    self._cache.clear()
    self._cache_bytes = 0
    self._summary = None
    self._generation += 1
    self._oversized.clear()
    self._blocks = self._settled_snapshot = self._pending_snapshot = ()
    self._live_snapshot = ()
    self._live_source = ()
    self._user_position = None

  @contextlib.contextmanager
  def _layout_pass(self) -> Generator[None]:
    """Reuse oversized boundary layouts until the pending layout is painted."""
    self._layout_ready = True
    try:
      yield
    finally:
      if self._layout_ready:
        self._oversized.clear()

  def _rows(self, index: int, part: int) -> _Rows:
    block = self._blocks[index]
    renderable = block.parts[part]
    # Match inline scrollback: one empty row between blocks or Markdown parts.
    padding = 1
    theme_name = self._theme_name()
    key = (block.key, block.revision, part, self._width, theme_name, padding)
    cached = self._cache.get(key)
    if cached is not None:
      self._cache.move_to_end(key)
      return cached[0]
    for oversized_key, oversized_rows in self._oversized:
      if oversized_key == key:
        return oversized_rows
    app = current.get_app_or_none()
    # Input geometry and plain status decorations must be available immediately.
    if (
      app is not None
      and app.is_running
      and not isinstance(renderable, (rendering.UserText, text.Text))
    ):
      self._render_needed = self._render_needed or (block, key)
      # Keep the last painted revision while formatting the newest snapshot.
      for previous, (rows, _) in reversed(self._cache.items()):
        if previous[0] == key[0] and previous[2:] == key[2:]:
          return rows
      for previous, rows in reversed(self._oversized):
        if previous[0] == key[0] and previous[2:] == key[2:]:
          return rows
      self._render_needed = (block, key)
      self._layout_ready = False
      return ((),) * padding
    rows, size = self._render_block(block, key, next(self._copy_ids))
    self._remember_rows(key, rows, size)
    return rows

  def _schedule_render(self) -> None:
    # One render at a time; prioritize unseen parts over refreshing live text.
    if self._render_task is None and self._render_needed is not None:
      block, key = self._render_needed
      self._render_task = current.get_app().create_background_task(
        self._render(block, key, self._generation)
      )

  async def _render(
    self, block: transcript.Block, key: _LayoutKey, generation: int
  ) -> None:
    try:
      # Rich layout and ANSI decoding must not hold up input or RPC cancellation.
      rows, size = await asyncio.to_thread(
        self._render_block, block, key, next(self._copy_ids)
      )
      if generation == self._generation:
        self._remember_rows(key, rows, size)
    finally:
      self._render_task = None
      current.get_app().invalidate()

  def _remember_rows(self, key: _LayoutKey, rows: _Rows, size: int) -> None:
    if size > self._max_cache_bytes:
      self._oversized.append((key, rows))
      return
    self._cache[key] = (rows, size)
    self._cache_bytes += size
    while self._cache and (
      len(self._cache) > max(64, 2 * self._height)
      or self._cache_bytes + sys.getsizeof(self._cache) > self._max_cache_bytes
    ):
      _, (_, removed_size) = self._cache.popitem(last=False)
      self._cache_bytes -= removed_size
    if not self._cache:
      self._cache.clear()
      self._oversized.append((key, rows))

  def _render_block(
    self, block: transcript.Block, key: _LayoutKey, copy_id: int
  ) -> tuple[_Rows, int]:
    """Render captured geometry and copy IDs without mutating viewport state."""
    _, _, part, width, theme, padding = key
    painter = rendering.ansi_console(
      io.StringIO(),
      width,
      theme_name=theme,
      interactive=not block.key.startswith('live:'),
    )
    fragments: formatted_text.StyleAndTextTuples = []
    marker = ''
    handlers: dict[bool, Callable[[mouse_events.MouseEvent], None]] = {}
    # Direct rendering bypasses the console's default text style.
    for index, segment in enumerate(
      painter.render(styled.Styled(block.parts[part], 'ui.text'))
    ):
      rich_style = segment.style
      value = segment.text
      metadata = rich_style.meta if rich_style else {}
      source = metadata.get('copy_code')
      if isinstance(source, str):
        marker = f' class:code-copy-{copy_id}-{index} '
        handlers = {
          False: functools.partial(self._code_event, source, marker, False),
          True: functools.partial(self._code_event, source, marker, True),
        }
      if rich_style:
        if theme == 'mono':
          rich_style = rich_style.without_color
        value = rich_style.render(
          value,
          color_system=color.ColorSystem.STANDARD
          if theme == 'terminal'
          else color.ColorSystem.TRUECOLOR,
        )
      decoded = formatted_text.to_formatted_text(hyperlinks.ANSI(value))
      if marker and any(
        key in metadata
        for key in ('copy_code', 'code_block', 'code_action', 'code_hint')
      ):
        action = bool(metadata.get('code_action'))
        role = ' class:code-action' if action else ''
        if metadata.get('code_hint'):
          role = ' class:code-hint'
        handler = handlers[action]
        fragments.extend(
          (style + marker + role, text, handler) for style, text, *_ in decoded
        )
      else:
        fragments.extend(decoded)
    rows = _split_rows(fragments, padding)
    return rows, _layout_bytes(key, rows)

  @override
  def clear_hover(self) -> None:
    if self._hovered_style is not None:
      self._hovered_style = None
      self._hovered_action = False
      current.get_app().invalidate()

  def _code_event(
    self, text: str, style: str, action: bool, event: mouse_events.MouseEvent
  ) -> None:
    if (
      event.event_type == mouse_events.MouseEventType.MOUSE_MOVE
      and event.button == mouse_events.MouseButton.NONE
      and not event.modifiers
    ):
      self._hovered_style = style
      self._hovered_action = action
    elif (
      action
      and self._hovered_style == style
      and event.event_type == mouse_events.MouseEventType.MOUSE_UP
      and event.button == mouse_events.MouseButton.LEFT
      and not event.modifiers
    ):
      try:
        workspace_application.copy_text(text)
      except ValueError:
        self._copy_label = 'Too large'
      else:
        self._copy_label = 'Copied'
      self._copied_style = style
      self._copy_until = time.monotonic() + 1.5
      app = current.get_app()
      if app.loop is not None:
        app.loop.call_later(1.5, app.invalidate)

  def _move(
    self, index: int, part: int, row: int, amount: int
  ) -> tuple[int, int, int]:
    row += amount
    while row < 0 and (index > 0 or part > 0):
      if part > 0:
        part -= 1
      else:
        index -= 1
        part = len(self._blocks[index].parts) - 1
      row += len(self._rows(index, part))
    while row >= len(self._rows(index, part)):
      if part + 1 < len(self._blocks[index].parts):
        row -= len(self._rows(index, part))
        part += 1
      elif index + 1 < len(self._blocks):
        row -= len(self._rows(index, part))
        index, part = index + 1, 0
      else:
        break
    return index, part, max(0, min(row, len(self._rows(index, part)) - 1))

  def _start(self) -> tuple[int, int, int]:
    if self._anchor is None:
      index = len(self._blocks) - 1
      part = len(self._blocks[index].parts) - 1
      return self._move(
        index, part, len(self._rows(index, part)) - 1, -self._height + 1
      )
    index = self._anchor_index
    if (
      index >= len(self._blocks)
      or self._blocks[index].key != self._anchor.block
    ):
      index = next(
        (
          i
          for i, block in enumerate(self._blocks)
          if block.key == self._anchor.block
        ),
        0,
      )
      self._anchor_index = index
    part = min(self._anchor.part, len(self._blocks[index].parts) - 1)
    if self._settling_line is not None:
      pattern, occurrence, offset, whole_record = self._settling_line
      parts = range(len(self._blocks[index].parts)) if whole_record else (part,)
      rows = [self._rows(index, i) for i in parts]
      if not self._layout_ready:
        return index, part, 0
      self._settling_line = None
      text = '\n'.join(map(_plain, itertools.chain.from_iterable(rows)))
      match = (
        next(
          itertools.islice(re.finditer(pattern, text), occurrence, None), None
        )
        if pattern
        else None
      )
      if match is not None:
        row = max(0, text.count('\n', 0, match.start()) - offset)
        for part_index, chunk in zip(parts, rows, strict=True):
          if row < len(chunk):
            part = part_index
            self._anchor = Position(self._blocks[index].key, row, part)
            break
          row -= len(chunk)
    return index, part, min(self._anchor.row, len(self._rows(index, part)) - 1)

  def _owner(
    self, index: int, part: int, row: int
  ) -> rendering.UserText | None:
    """The user message whose box has crossed the viewport's top edge."""
    user_index = self._user_before(index)
    if user_index is None or (user_index == index and part == 0 and row == 0):
      return None
    message = self._blocks[user_index].renderable
    return message if isinstance(message, rendering.UserText) else None

  def _header(self, owner: rendering.UserText) -> _Rows:
    """Summarize the original message, with a blank separator when space allows."""
    cached = self._summary
    # Identity avoids hashing or comparing multi-megabyte prompts on redraw.
    if (
      cached is None
      or cached[0] is not owner.content
      or cached[1] != self._width
    ):
      cached = self._summary = (
        owner.content,
        self._width,
        _summarize(owner.content, self._width),
      )
    header = ((('class:ui.muted.italic', cached[2]),),)
    return header + ((),) if self._height > 2 else header

  def _content_start(self) -> tuple[int, int, int]:
    """The first conversation row painted below any pinned header."""
    index, part, row = self._start()
    if self._anchor is not None:
      return index, part, row
    owner = self._owner(index, part, row)
    if owner is None:
      return index, part, row
    return self._move(index, part, row, len(self._header(owner)))

  def _shows_last_row(self, index: int, part: int, row: int) -> bool:
    """Whether a viewport whose content starts here reaches the final row."""
    owner = self._owner(index, part, row)
    header_height = 0 if owner is None else len(self._header(owner))
    end_index, end_part, end_row = self._move(
      index, part, row, self._height - header_height - 1
    )
    return (
      end_index == len(self._blocks) - 1
      and end_part == len(self._blocks[end_index].parts) - 1
      and end_row == len(self._rows(end_index, end_part)) - 1
    )

  def scroll(self, rows: int) -> None:
    """Move by terminal rows, resuming following when the final row is visible."""
    self.clear_hover()
    if not self._blocks or (rows > 0 and self._anchor is None):
      return
    with self._layout_pass():
      index, part, row = self._move(*self._content_start(), rows)
      if self._shows_last_row(index, part, row):
        self.follow()
        return
      self._anchor_index = index
      self._anchor = Position(self._blocks[index].key, row, part)

  def page(self, direction: int) -> None:
    """Scroll one viewport, retaining a row of context."""
    self.scroll(direction * max(1, self._height - self._header_height - 1))

  def navigate(self, action: actions.NavigateHistory) -> None:
    """Apply reducer-selected navigation without taking editor focus."""
    match action.direction:
      case 'page_up':
        self.page(-1)
      case 'page_down':
        self.page(1)
      case 'oldest':
        self.first()
      case 'latest':
        self.follow()
      case 'up':
        self.scroll(-3)
      case 'down':
        self.scroll(3)

  def _refresh_blocks(self) -> None:
    settled = self._source()
    pending = tuple(self._pending())
    live_source = self._live()
    if (
      isinstance(live_source, tuple)
      and live_source is self._live_source
      and settled is self._settled_snapshot
      and pending == self._pending_snapshot
    ):
      return
    live_blocks: list[transcript.Block] = []
    for index, renderable in enumerate(live_source):
      previous = (
        self._live_snapshot[index] if index < len(self._live_snapshot) else None
      )
      if previous is not None and previous.renderable is renderable:
        live_blocks.append(previous)
      else:
        self._live_revision += 1
        live_blocks.append(
          transcript.Block(
            f'live:{index}', (0, self._live_revision), renderable
          )
        )
    self._live_source = live_source
    if settled is self._settled_snapshot and pending == self._pending_snapshot:
      if len(live_blocks) == len(self._live_snapshot) and all(
        block is previous
        for block, previous in zip(
          live_blocks, self._live_snapshot, strict=True
        )
      ):
        return
    else:
      self._user_position = None
    if self._user_position is not None:
      first, last, user = self._user_position
      last = min(last, len(settled) + len(pending) - 1)
      self._user_position = (first, last, user) if first <= last else None
    self._live_snapshot = tuple(live_blocks)
    self._settled_snapshot = settled
    self._pending_snapshot = pending
    self._blocks = settled + pending + self._live_snapshot

  def _user_before(self, index: int) -> int | None:
    if self._user_position is not None:
      first, last, user = self._user_position
      if first <= index <= last:
        return user
      if index > last:
        for position in range(last + 1, index + 1):
          if isinstance(self._blocks[position].renderable, rendering.UserText):
            first = user = position
        self._user_position = (first, index, user)
        return user
    user = next(
      (
        i
        for i in range(index, -1, -1)
        if isinstance(self._blocks[i].renderable, rendering.UserText)
      ),
      None,
    )
    self._user_position = (user or 0, index, user)
    return user

  @override
  def create_content(self, width: int, height: int) -> controls.UIContent:
    with self._layout_pass():
      if (
        width != self._width
        and self._anchor is not None
        and self._blocks
        and self._settling_line is None
      ):
        self._remember_line(*self._start())
      if (width, height) != (self._width, self._height):
        self.clear_hover()
      self._render_needed = None
      self._layout_ready = True
      self._width, self._height = max(1, width), max(1, height)
      self._refresh_blocks()
      visible: list[_Row] = []
      self._header_height = 0
      if self._blocks:
        index, part, row = self._start()
        owner = self._owner(index, part, row)
        header = () if owner is None else self._header(owner)
        self._header_height = len(header)
        if self._anchor is None:
          index, part, row = self._move(index, part, row, len(header))
        visible.extend(header)
        while len(visible) < height and index < len(self._blocks):
          rows = self._rows(index, part)
          while row < len(rows) and len(visible) < height:
            visible.append(rows[row])
            row += 1
          if part + 1 < len(self._blocks[index].parts):
            part, row = part + 1, 0
          else:
            index, part, row = index + 1, 0, 0
      snapshot = tuple(visible) if self._layout_ready else self._visible
      if snapshot != self._visible:
        self.clear_hover()
      self._visible = snapshot
      self._schedule_render()
      return controls.UIContent(
        get_line=lambda index: self._display_row(snapshot[index]),
        line_count=len(snapshot),
        show_cursor=False,
      )

  def _display_row(self, fragments: _Row) -> formatted_text.StyleAndTextTuples:
    if self._hovered_style is None:
      return list(fragments)
    if time.monotonic() >= self._copy_until:
      self._copied_style = None
    row: formatted_text.StyleAndTextTuples = []
    for fragment in fragments:
      style, text = fragment[:2]
      if self._hovered_style and self._hovered_style in style:
        copied = self._copied_style == self._hovered_style
        if 'class:code-action' in style:
          style += self._hovered_style.replace('code-copy-', 'click-target-')
          style += ' class:mouse-pointer'
          if self._hovered_action:
            style += ' class:code-action-hover'
          icon = (
            ('✓' if self._copy_label == 'Copied' else '!') if copied else '⧉'
          )
          text = icon.center(len(text))
        elif 'class:code-hint' in style and (copied or self._hovered_action):
          label = self._copy_label if copied else 'click to copy'
          if len(label) <= len(text):
            text = label.rjust(len(text))
        if copied and (
          'class:code-action' in style or 'class:code-hint' in style
        ):
          style += (
            ' class:code-copied'
            if self._copy_label == 'Copied'
            else ' class:code-copy-error'
          )
      row.append(
        (style, text, fragment[2]) if len(fragment) == 3 else (style, text)
      )
    return row

  @override
  def mouse_handler(self, mouse_event: mouse_events.MouseEvent) -> None:
    if mouse_event.event_type == mouse_events.MouseEventType.MOUSE_MOVE:
      self.clear_hover()
    if mouse_event.event_type == mouse_events.MouseEventType.SCROLL_UP:
      self._navigate(actions.NavigateHistory('up'))
    elif mouse_event.event_type == mouse_events.MouseEventType.SCROLL_DOWN:
      self._navigate(actions.NavigateHistory('down'))
    elif 0 <= mouse_event.position.y < len(self._visible):
      column = 0
      for fragment in self._visible[mouse_event.position.y]:
        if '[ZeroWidthEscape]' in fragment[0]:
          continue
        end = column + len(fragment[1])
        if column <= mouse_event.position.x < end and len(fragment) == 3:
          fragment[2](mouse_event)
          break
        column = end
    current.get_app().invalidate()
