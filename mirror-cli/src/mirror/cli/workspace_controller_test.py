"""Launch preferences and reopened tabs survive discovery and tab switching."""

from __future__ import annotations

import asyncio
import pathlib
import subprocess
import sys
from unittest import mock

import pytest
from mirror.protocol import (
  agent_pb2,
  client_pb2,
  mirror_pb2,
)

from mirror import config
from mirror.cli import (
  actions,
  cli,
  cli_store,
  extension,
  main,
  models,
  onboarding,
  workspace_controller,
  workspaces,
  worktrees,
)


@pytest.mark.parametrize(
  ('flags', 'saved_effort', 'expected_approval', 'expected_effort'),
  [
    ((), None, 'auto', 'max'),
    ((), 'low', 'auto', 'low'),
    (('--approval-mode', 'approve'), 'low', 'approve', 'low'),
    (('--approval-mode', 'auto'), 'low', 'auto', 'low'),
    (('--reasoning-effort', 'low'), 'max', 'auto', 'low'),
    (('--reasoning-effort', 'medium'), 'max', 'auto', 'medium'),
    (('--reasoning-effort', 'xhigh'), 'max', 'auto', 'xhigh'),
    (('--reasoning-effort', 'max'), 'low', 'auto', 'max'),
  ],
)
@pytest.mark.parametrize('create_session', [False, True])
@pytest.mark.asyncio
async def test_launch_reopens_workspace_and_honors_preference_precedence(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  flags: tuple[str, ...],
  saved_effort: str | None,
  expected_approval: str,
  expected_effort: str,
  create_session: bool,
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
        approval_mode=agent_pb2.APPROVAL_MODE_AUTO,
        definition=mirror_pb2.AgentDefinition(reasoning_effort=saved_effort),
      ),
    )
    store.set_worktree_preferences(
      str(primary),
      str(feature),
      client_pb2.WorkspacePreferences(
        definition=mirror_pb2.AgentDefinition(reasoning_effort='high'),
      ),
    )
    store.close_workspace(str(primary), str(primary))

  args = main._parse_args()  # pyright: ignore[reportPrivateUsage]
  worker_config = main._worker_config(  # pyright: ignore[reportPrivateUsage]
    args,
    'launch',
    create_session=create_session,
    allow_workspace_mismatch=False,
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
    reasoning_effort=args.reasoning_effort,
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
    assert launch.agent.approval_mode == expected_approval
    assert launch.agent.reasoning_effort == expected_effort
    assert isinstance(launch.agent, cli._Repl)  # pyright: ignore[reportPrivateUsage]
    inference = launch.agent._worker_configuration.inference_config(  # pyright: ignore[reportPrivateUsage]
      launch.agent.model, launch.agent.reasoning_effort, remote=False
    )
    assert inference.reasoning_effort == expected_effort
    assert inference.api == 'reflection'
    assert inference.model == 'Beam-1.0-501B-A23B'
    await controller._reconcile()  # pyright: ignore[reportPrivateUsage]
    assert await controller.select(1)
    task = controller._reconcile_task  # pyright: ignore[reportPrivateUsage]
    assert task is not None
    await task
    assert launch in controller.visible_workspaces()
    assert controller.selected is not launch
    assert isinstance(controller.selected, workspaces.AgentWorkspace)
    assert controller.selected.agent.reasoning_effort == 'high'
    with cli_store.Store(database) as store:
      assert str(primary) not in store.closed_workspaces(str(primary))
      saved = store.worktree_overlays(str(primary))[str(primary)].preferences
      assert saved.definition.reasoning_effort == (saved_effort or '')
  finally:
    await controller.close()
    await registry.close()


@pytest.mark.parametrize('discovery_fails', [False, True])
@pytest.mark.asyncio
async def test_normal_workspace_reuses_managed_worktree_without_granting_trust(
  tmp_path: pathlib.Path,
  monkeypatch: pytest.MonkeyPatch,
  discovery_fails: bool,
) -> None:
  monkeypatch.setenv('HOME', str(tmp_path))
  monkeypatch.setenv('MIRROR_MODELS_CONFIG', str(tmp_path / 'models.json'))
  primary = tmp_path / 'primary'
  primary.mkdir()
  for arguments in (
    ('init', '-b', 'main'),
    (
      '-c',
      'user.name=Test',
      '-c',
      'user.email=test@example.com',
      'commit',
      '--allow-empty',
      '-m',
      'Initial',
    ),
    ('remote', 'add', 'origin', str(primary)),
  ):
    subprocess.run(
      ['git', '-C', str(primary), *arguments], check=True, capture_output=True
    )
  onboarding.ensure_consent(primary, confirm=lambda _: True)

  def create_agent(
    spec: workspaces.AgentSpec,
    controller: workspace_controller.WorkspaceController,
  ) -> workspaces.Agent:
    del controller
    return mock.Mock(
      spec=workspaces.Agent,
      workspace=spec.workspace,
      model='test',
      model_selection=models.ModelOption('test', 'test'),
      reasoning_effort=spec.reasoning_effort,
      approval_mode='auto',
      conversation_id=str(spec.config['conversation_id']),
      activity_status='',
      queued_message_count=0,
      worktree_state=actions.WorktreeAgentState.NOT_STARTED,
      start=mock.AsyncMock(),
      close=mock.AsyncMock(),
    )

  controller = workspace_controller.WorkspaceController(
    {'conversation_id': 'test', 'model': 'test', 'approval_mode': 'auto'},
    workspace=primary,
    reasoning_effort='medium',
    database=tmp_path / 'cli.db',
    create_agent=create_agent,
  )
  try:
    worktree_id = await controller.create_workspace('worktree')
    assert await controller.select_workspace(worktree_id)
    async with asyncio.timeout(5):
      while isinstance(controller.selected, workspaces.PendingWorkspace):
        assert controller.selected.error is None
        await asyncio.sleep(0)
    checkout = controller.selected.path
    assert checkout is not None
    assert worktrees.is_managed_directory(checkout)
    assert not onboarding.is_trusted_directory(checkout)

    normal_id = await controller.create_workspace('normal')
    assert normal_id != worktree_id
    assert await controller.select_workspace(normal_id)
    assert controller.selected.path == checkout
    assert not onboarding.is_trusted_directory(checkout)

    untrusted = tmp_path / 'untrusted'
    untrusted.mkdir()
    with pytest.raises(ValueError, match='not trusted'):
      await controller.open_folder(normal_id, untrusted)
    with pytest.raises(ValueError, match='not trusted'):
      await controller.open_folder(normal_id, checkout)
  finally:
    await controller.close()

  unrelated = tmp_path / '.mirror' / 'worktrees' / 'unrelated'
  unrelated.mkdir(parents=True)
  subprocess.run(
    ['git', '-C', str(unrelated), 'init', '-b', 'main'],
    check=True,
    capture_output=True,
  )
  assert worktrees.is_managed_directory(unrelated)
  with cli_store.Store(tmp_path / 'cli.db') as store:
    store.save_workspace(
      str(primary / '.git'), 'untrusted', str(untrusted), 'Untrusted'
    )
    store.save_workspace(
      str(primary / '.git'), 'unrelated', str(unrelated), 'Unrelated'
    )

  def discover(path: pathlib.Path) -> tuple[worktrees.Worktree, ...]:
    if discovery_fails:
      raise worktrees.WorktreeError('worktree listing unavailable')
    return worktrees.list_worktrees(path)

  restored = workspace_controller.WorkspaceController(
    {'conversation_id': 'restart', 'model': 'test', 'approval_mode': 'auto'},
    discover=discover,
    workspace=primary,
    reasoning_effort='medium',
    database=tmp_path / 'cli.db',
    create_agent=create_agent,
  )
  try:
    assert await restored.select_workspace(normal_id) is not discovery_fails
    assert restored.selected.path == (primary if discovery_fails else checkout)
    assert not onboarding.is_trusted_directory(checkout)
    assert not await restored.select_workspace('untrusted')
    assert not await restored.select_workspace('unrelated')
    assert isinstance(restored.selected, workspaces.AgentWorkspace)
    with pytest.raises(ValueError, match='not trusted'):
      await restored.open_folder(restored.selected.id, checkout)
  finally:
    await restored.close()
