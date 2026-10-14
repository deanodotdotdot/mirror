from __future__ import annotations

import pytest
from prompt_toolkit import application, buffer, keys, layout
from prompt_toolkit.input import base as input_base
from prompt_toolkit.input import defaults, vt100_parser
from prompt_toolkit.output import base as output_base

from mirror.cli import actions, extended_keys, keymap


def test_install_is_idempotent() -> None:
  extended_keys.install()

  assert extended_keys.install() == 0


def test_csi_u_aliases_match_mirror_keybindings() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('\x1b[49;5u\x1b[9;2u\x1b[100;5u\x1b[122;6u')

    presses = pipe.read_keys()

  assert [press.key for press in presses] == [
    keys.Keys.Control1,
    keys.Keys.BackTab,
    keys.Keys.ControlD,
    keys.Keys.ControlY,
  ]


def test_csi_u_escape_aliases_parse_as_escape() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('\x1b[27u\x1b[27;1u')

    presses = pipe.read_keys()

  assert [press.key for press in presses] == [
    keys.Keys.Escape,
    keys.Keys.Escape,
  ]


@pytest.mark.parametrize('escape', ['\x1b[27u', '\x1b[27;1u'])
@pytest.mark.asyncio
async def test_encoded_escape_interrupts_before_queued_text(
  escape: str,
) -> None:
  extended_keys.install()
  dispatched: list[actions.Action] = []
  mapping = keymap.Keymap(
    lambda: [(keymap.Binding(('escape',), actions.InterruptRequested()),)],
    dispatched.append,
    dismiss=lambda: None,
  )
  composer = buffer.Buffer()
  app = application.Application[None](
    layout=layout.Layout(layout.Window(layout.BufferControl(composer))),
    key_bindings=mapping.bindings,
    input=input_base.DummyInput(),
    output=output_base.DummyOutput(),
  )
  parser = vt100_parser.Vt100Parser(app.key_processor.feed)
  with application.set_app(app):
    parser.feed(escape + 'hello')
    app.key_processor.process_keys()
    assert dispatched == [actions.InterruptRequested()]
    assert composer.text == 'hello'
    assert app.key_processor.key_buffer == []
    await app.cancel_and_wait_for_background_tasks()


def test_modify_other_keys_aliases_match_mirror_keybindings() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('\x1b[27;5;50~\x1b[27;2;9~')

    presses = pipe.read_keys()

  assert [press.key for press in presses] == [
    keys.Keys.Control2,
    keys.Keys.BackTab,
  ]


def test_plain_text_keeps_its_text_data() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('I ^')

    presses = pipe.read_keys()

  assert [(press.key, press.data) for press in presses] == [
    ('I', 'I'),
    (' ', ' '),
    ('^', '^'),
  ]


def test_shift_enter_remains_distinct_from_plain_enter() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('\r\x1b[13;2u')

    presses = pipe.read_keys()

  assert [press.key for press in presses] == [
    keys.Keys.ControlM,
    keys.Keys.ControlJ,
  ]


def test_extended_sequence_can_arrive_in_fragments() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('\x1b[49;')
    assert pipe.read_keys() == []
    pipe.send_text('5u')

    presses = pipe.read_keys()

  assert [press.key for press in presses] == [keys.Keys.Control1]


def test_paste_keeps_extended_sequences_literal() -> None:
  extended_keys.install()
  with defaults.create_pipe_input() as pipe:
    pipe.send_text('\x1b[200~before\x1b[49;5uafter\x1b[201~')

    presses = pipe.read_keys()

  assert len(presses) == 1
  assert presses[0].key is keys.Keys.BracketedPaste
  assert presses[0].data == 'before\x1b[49;5uafter'
