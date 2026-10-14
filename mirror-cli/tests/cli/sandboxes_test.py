from __future__ import annotations

import pathlib
import subprocess

import pytest

from mirror.cli import sandboxes


def _git(cwd: pathlib.Path, *arguments: str) -> str:
  return subprocess.run(
    ['git', '-C', str(cwd), *arguments],
    check=True,
    capture_output=True,
    text=True,
  ).stdout.strip()


@pytest.fixture
def repository(tmp_path: pathlib.Path) -> pathlib.Path:
  repository = tmp_path / 'repository'
  repository.mkdir()
  _git(repository, 'init', '--quiet', '--initial-branch', 'main')
  _git(repository, 'config', 'user.name', 'Ada')
  _git(repository, 'config', 'user.email', 'ada@example.com')
  (repository / 'README').write_text('hello\n')
  _git(repository, 'add', 'README')
  _git(repository, 'commit', '--quiet', '--message', 'first')
  _git(
    repository, 'remote', 'add', 'origin', 'https://example.com/team/repo.git'
  )
  return repository


def test_create_clone_starts_a_branch_with_the_sources_upstream_and_author(
  tmp_path: pathlib.Path, repository: pathlib.Path
) -> None:
  path, name = sandboxes.create_clone(
    repository, 'scratch', root=tmp_path / 'sandboxes'
  )

  assert (path, name) == (
    (tmp_path / 'sandboxes' / 'scratch').resolve(),
    'scratch',
  )
  assert (path / 'README').read_text() == 'hello\n'
  assert _git(path, 'branch', '--show-current') == 'mirror/scratch'
  assert (
    _git(path, 'remote', 'get-url', 'origin')
    == 'https://example.com/team/repo.git'
  )
  assert _git(path, 'config', 'user.email') == 'ada@example.com'


@pytest.mark.parametrize(
  ('origin', 'expected'),
  [
    (
      'https://ada:token@example.com/team/repo.git',
      'https://example.com/team/repo.git',
    ),
    (
      'https://example.com/team/repo.git?access_token=secret',
      'https://example.com/team/repo.git',
    ),
    (
      'ssh://git:secret@example.com/team/repo.git',
      'ssh://git@example.com/team/repo.git',
    ),
  ],
)
def test_create_clone_leaves_the_origins_credentials_behind(
  tmp_path: pathlib.Path, repository: pathlib.Path, origin: str, expected: str
) -> None:
  _git(repository, 'remote', 'set-url', 'origin', origin)

  path, _ = sandboxes.create_clone(
    repository, 'scratch', root=tmp_path / 'sandboxes'
  )

  assert _git(path, 'remote', 'get-url', 'origin') == expected


def test_create_clone_starts_from_a_fresh_git_config(
  tmp_path: pathlib.Path, repository: pathlib.Path
) -> None:
  _git(
    repository,
    'config',
    'remote.origin.pushurl',
    'https://ada:synthetic-0000@example.com/team/repo.git',
  )

  path, _ = sandboxes.create_clone(
    repository, 'scratch', root=tmp_path / 'sandboxes'
  )

  assert 'synthetic-0000' not in (path / '.git' / 'config').read_text()


def test_create_clone_copies_objects_rather_than_linking_them(
  tmp_path: pathlib.Path, repository: pathlib.Path
) -> None:
  path, _ = sandboxes.create_clone(
    repository, 'scratch', root=tmp_path / 'sandboxes'
  )

  source = {
    file.stat().st_ino
    for file in (repository / '.git' / 'objects').rglob('*')
    if file.is_file()
  }
  clone = {
    file.stat().st_ino
    for file in (path / '.git' / 'objects').rglob('*')
    if file.is_file()
  }
  assert clone
  assert source.isdisjoint(clone)


def test_create_clone_refuses_a_name_that_is_not_a_plain_word(
  tmp_path: pathlib.Path, repository: pathlib.Path
) -> None:
  with pytest.raises(sandboxes.SandboxError, match='lowercase'):
    sandboxes.create_clone(repository, '../escape', root=tmp_path)


def test_create_clone_without_source_creates_empty_workspace(
  tmp_path: pathlib.Path,
) -> None:
  path, name = sandboxes.create_clone(None, 'empty', root=tmp_path)
  assert name == 'empty'
  assert list(path.iterdir()) == []


def test_sandbox_settings_survive_being_stored() -> None:
  sandbox = sandboxes.Sandbox.from_settings({
    'sandbox': {
      'image': 'python:3.12-bookworm',
      'egress': ['github.com'],
      'cpus': 4,
      'memory_mib': 4096,
    }
  })

  assert sandboxes.Sandbox.from_json(sandbox.to_json()) == sandbox
  assert (sandbox.image, sandbox.egress, sandbox.cpus, sandbox.memory_mib) == (
    'python:3.12-bookworm',
    ('github.com',),
    4,
    4096,
  )


def test_sandbox_without_settings_uses_the_defaults() -> None:
  sandbox = sandboxes.Sandbox.from_settings({})

  assert (sandbox.image, sandbox.egress) == (
    sandboxes.DEFAULT_IMAGE,
    sandboxes.DEFAULT_EGRESS,
  )


@pytest.mark.parametrize(
  ('setting', 'value'), [('cpus', 256), ('memory_mib', 2**32)]
)
def test_sandbox_settings_reject_a_size_the_worker_cannot_read(
  setting: str, value: int
) -> None:
  with pytest.raises(sandboxes.SandboxError, match=setting):
    sandboxes.Sandbox.from_settings({'sandbox': {setting: value}})


def test_sandbox_settings_reject_egress_that_is_not_a_list() -> None:
  with pytest.raises(sandboxes.SandboxError, match='egress'):
    sandboxes.Sandbox.from_settings({'sandbox': {'egress': 'github.com'}})
