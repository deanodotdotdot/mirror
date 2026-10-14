"""Launch preferences and reopened tabs survive discovery and tab switching."""

from __future__ import annotations

import asyncio
import pathlib
import sys

import pytest
from mirror.protocol import (
  agent_pb2,
  client_pb2,
)

from mirror import config
from mirror.cli import (
  cli,
  cli_store,
  extension,
  main,
  workspace_controller,
  workspaces,
  worktrees,
)


@pytest.mark.parametrize(
  ('flags', 'expected'),
  [
    ((), 'auto'),
    (('--approval-mode', 'approve'), 'approve'),
    (('--approval-mode', 'auto'), 'auto'),
  ],
)
@pytest.mark.asyncio
async def test_launch_reopens_workspace_and_honors_approval_precedence(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  flags: tuple[str, ...],
  expected: str,
) -> None:
  primary = tmp_path / 'primary'
  feature = tmp_path / 'feature'
  primary.mkdir()
  feature.mkdir()
  database = tmp_path / 'cli.db'
  monkeypatch.setenv('HOME', str(tmp_path))
  monkeypatch.setenv('MIRROR_CLI_DB', str(database))
  monkeypatch.setenv('MIRROR_STATE_DB', str(tmp_path / 'state.db'))
  monkeypatch.setenv('MIRROR_MODELS_CONFIG', str(tmp_path / 'models.json'))
  monkeypatch.setenv('MIRROR_EXTENSIONS_ENABLED', 'false')
  monkeypatch.chdir(primary)
  monkeypatch.setattr(sys, 'argv', ['mirror', *flags])
  config.save_approval_mode('approve')
  with cli_store.Store(database) as store:
    store.set_worktree_preferences(
      str(primary),
      str(primary),
      client_pb2.WorkspacePreferences(
        approval_mode=agent_pb2.APPROVAL_MODE_AUTO
      ),
    )
    store.close_workspace(str(primary), str(primary))

  args = main._parse_args()  # pyright: ignore[reportPrivateUsage]
  worker_config = main._worker_config(  # pyright: ignore[reportPrivateUsage]
    args, 'launch', create_session=True, allow_workspace_mismatch=False
  )
  registry = extension.ExtensionRegistry()

  def create_agent(
    spec: workspaces.AgentSpec,
    controller: workspace_controller.WorkspaceController,
  ) -> workspaces.Agent:
    return cli._Repl(  # pyright: ignore[reportPrivateUsage]
      [sys.executable],
      spec.config,
      session_id='test',
      workspace=spec.workspace,
      workspace_id=spec.workspace_id,
      database=database,
      reasoning_effort=spec.reasoning_effort,
      extension_registry=registry,
      workspace_controller=controller,
      workspace_effects=asyncio.Queue(),
      active=spec.active,
    )

  controller = workspace_controller.WorkspaceController(
    worker_config,
    workspace=primary,
    reasoning_effort='medium',
    database=database,
    create_agent=create_agent,
    display_unmanaged_worktrees=True,
    discover=lambda _: (
      worktrees.Worktree(primary, None),
      worktrees.Worktree(feature, None),
    ),
  )
  try:
    launch = controller.selected
    assert isinstance(launch, workspaces.AgentWorkspace)
    assert launch.agent.approval_mode == expected
    await controller._reconcile()  # pyright: ignore[reportPrivateUsage]
    assert await controller.select(1)
    task = controller._reconcile_task  # pyright: ignore[reportPrivateUsage]
    assert task is not None
    await task
    assert launch in controller.visible_workspaces()
    assert controller.selected is not launch
    with cli_store.Store(database) as store:
      assert str(primary) not in store.closed_workspaces(str(primary))
  finally:
    await controller.close()
    await registry.close()
