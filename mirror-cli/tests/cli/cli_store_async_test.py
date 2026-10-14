"""Database waits leave the UI loop free, but do not outlive cancellation."""

import asyncio
import datetime
import pathlib
import sqlite3
import threading

import pytest

from mirror.cli import cli_store


def _record(store: cli_store.Store, directory: pathlib.Path) -> None:
  store.record_user_message(
    'session',
    entry_id='user',
    sequence=1,
    timestamp=datetime.datetime(2026, 1, 1, tzinfo=datetime.UTC),
    content='saved',
    cwd=str(directory),
    model='test',
  )


@pytest.mark.asyncio
async def test_call_keeps_loop_responsive_while_waiting_for_database(
  tmp_path: pathlib.Path,
) -> None:
  path = tmp_path / 'cli.db'
  with cli_store.Store(path):
    pass
  loop = asyncio.get_running_loop()
  started = asyncio.Event()

  def write(store: cli_store.Store) -> None:
    loop.call_soon_threadsafe(started.set)
    _record(store, tmp_path)

  with sqlite3.connect(path) as locking:
    locking.execute('BEGIN IMMEDIATE')
    writing = asyncio.create_task(cli_store.call(path, write))
    try:
      await asyncio.wait_for(started.wait(), timeout=2)
      assert not writing.done(), 'UI must run before the locked write finishes'
    finally:
      locking.rollback()
      await writing
  with cli_store.Store(path) as store:
    assert store.recent_user_messages('session') == ['saved']


@pytest.mark.asyncio
async def test_call_cancellation_waits_for_thread_owned_write(
  tmp_path: pathlib.Path,
) -> None:
  path = tmp_path / 'cli.db'
  loop = asyncio.get_running_loop()
  started = asyncio.Event()
  release = threading.Event()

  def write(store: cli_store.Store) -> None:
    loop.call_soon_threadsafe(started.set)
    assert release.wait(timeout=5), 'Test did not release the database worker'
    _record(store, tmp_path)

  writing = asyncio.create_task(cli_store.call(path, write))
  try:
    await asyncio.wait_for(started.wait(), timeout=2)
    writing.cancel()
    await asyncio.sleep(0)
    writing.cancel()
    await asyncio.sleep(0)
    assert not writing.done(), 'Cancellation must not orphan the write'
  finally:
    release.set()
    with pytest.raises(asyncio.CancelledError):
      await writing
  with cli_store.Store(path) as store:
    assert store.recent_user_messages('session') == ['saved']


@pytest.mark.asyncio
async def test_call_propagates_persistence_failures(
  tmp_path: pathlib.Path,
) -> None:
  with pytest.raises(cli_store.ConversationNotFoundError):
    await cli_store.call(
      tmp_path / 'cli.db', lambda store: store.get_info('absent')
    )
