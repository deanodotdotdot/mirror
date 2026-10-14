"""Repository reads follow host visibility even when tabs share a directory."""

from __future__ import annotations

import asyncio
import dataclasses
import pathlib
import subprocess
from unittest import mock

import pytest
import pytest_asyncio

from mirror.cli import actions, effects, extension, shell, state
from mirror.cli.extensions.repository_status import (
  extension as repository_status,
)


class _Tab:
  def __init__(
    self, registry: extension.ExtensionRegistry, directory: pathlib.Path
  ) -> None:
    self.state: state.State
    self.reads: list[None] = []
    self.changed = asyncio.Event()
    context = extension.Context(
      extension.Workspace(
        directory,
        remote=False,
        shell=shell.Shell(directory, validate=lambda: self.reads.append(None)),
      ),
      dispatch=self.dispatch,
      get_state=lambda: self.state,
      enqueue=lambda _: None,
      refresh_tools=registry.refresh_tools,
      call_tool=lambda _name, _arguments: pytest.fail('Unexpected tool call'),
      transcript=tuple,
      builtins=tuple,
      extension_tools=tuple,
      workspace_controller=mock.Mock(),
      create_agent=lambda _options, _message, _error: pytest.fail(
        'Unexpected agent creation'
      ),
    )
    self.host = registry.create_host(context)
    self.state = state.State(
      activity=state.ActivityState(),
      status_bar=state.StatusBarState('model', 'medium', 'auto'),
      session=state.SessionState('conversation'),
      lifecycle=state.LifecycleState(active=True),
      workspace=str(directory),
      extensions=self.host.initial_states,
    )

  def dispatch(self, action: actions.Action) -> None:
    states, pending = self.host.reduce(self.state, action)
    self.state = dataclasses.replace(self.state, extensions=states)
    for effect in pending:
      assert isinstance(effect, effects.RunExtensionEffect)
      self.host.handle_effect(effect)
    if isinstance(action, actions.RoutedExtensionAction):
      self.changed.set()

  async def wait_for_update(self) -> None:
    await asyncio.wait_for(self.changed.wait(), timeout=3)
    self.changed.clear()

  def finish_edit(self) -> None:
    self.state = dataclasses.replace(
      self.state,
      stream=state.StreamState(
        running=(actions.RunningToolCall('edit', 'edit', '', 'tool', '{}'),)
      ),
    )
    self.dispatch(actions.FinishRunningToolCall('edit'))


@pytest_asyncio.fixture
async def registry() -> extension.ExtensionRegistry:
  registry = extension.ExtensionRegistry()
  await registry.register(repository_status.RepositoryStatusExtension(), None)
  return registry


@pytest.fixture
def repository(tmp_path: pathlib.Path) -> pathlib.Path:
  subprocess.run(
    ('git', 'init', '--initial-branch=main', str(tmp_path)),
    check=True,
    capture_output=True,
  )
  (tmp_path / 'tracked').write_text('initial\n')
  subprocess.run(('git', '-C', str(tmp_path), 'add', 'tracked'), check=True)
  return tmp_path


@pytest.mark.asyncio
async def test_shared_folder_tabs_keep_visibility_and_refreshes_independent(
  registry: extension.ExtensionRegistry, repository: pathlib.Path
) -> None:
  first = _Tab(registry, repository)
  second = _Tab(registry, repository)
  try:
    await first.host.start()
    await second.host.start()
    await first.host.foreground()
    await first.wait_for_update()
    await asyncio.sleep(0.3)
    assert second.reads == []

    await first.host.background()
    await second.host.foreground()
    await second.wait_for_update()
    await first.host.close()
    (repository / 'tracked').write_text('edited\nextra\n')
    second.finish_edit()
    await second.wait_for_update()
    observed = second.state.extensions['core/status']
    assert isinstance(observed, repository_status.RepositoryStatus)
    assert observed.additions == 2

    await second.host.background()
    (repository / 'tracked').write_text('edited again\nextra\nthird\n')
    await second.host.foreground()
    await second.wait_for_update()
    observed = second.state.extensions['core/status']
    assert isinstance(observed, repository_status.RepositoryStatus)
    assert observed.additions == 3
  finally:
    await first.host.close()
    await second.host.close()
    await registry.close()


@pytest.mark.asyncio
async def test_hiding_during_debounce_defers_reads_until_next_foreground(
  registry: extension.ExtensionRegistry, repository: pathlib.Path
) -> None:
  tab = _Tab(registry, repository)
  try:
    await tab.host.start()
    await tab.host.foreground()
    await asyncio.sleep(0.05)
    await tab.host.background()
    tab.finish_edit()
    await asyncio.sleep(0.3)
    assert tab.reads == []
    await tab.host.foreground()
    await tab.wait_for_update()
    assert tab.reads
  finally:
    await tab.host.close()
    await registry.close()
