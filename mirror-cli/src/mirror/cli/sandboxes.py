"""Sandbox workspaces: a private clone whose tools run in a microsandbox VM.

The worker stays on this machine and runs its workspace tools through
`sandboxd` in a VM that mounts the clone at the same path. The clone is kept
apart from the user's checkout because the guest can write any file in it,
including its Git configuration. Once a VM can reach the clone, Git commands
that run configured programs, such as `status` or `add`, must not run in it
on this machine: they would obey the hooks, filters and helpers the guest sets.
"""

from __future__ import annotations

import contextlib
import dataclasses
import json
import os
import pathlib
import re
import shutil
import subprocess
import urllib.parse
import uuid
from collections.abc import Mapping
from typing import cast

from mirror.cli import worktrees

DEFAULT_IMAGE = 'python:3.12-bookworm'
# Hosts the VM may reach over HTTPS; all other traffic is denied.
DEFAULT_EGRESS = (
  'github.com',
  'api.github.com',
  'codeload.github.com',
  'objects.githubusercontent.com',
  'pypi.org',
  'files.pythonhosted.org',
  'registry.npmjs.org',
)
DEFAULT_CPUS = 2
DEFAULT_MEMORY_MIB = 2048
# The worker reads `cpus` as a `u8` and `memory_mib` as a `u32`.
_MAX_CPUS = 255
_MAX_MEMORY_MIB = 2**32 - 1
_SAFE_NAME = re.compile(r'^[a-z0-9][a-z0-9-]{0,39}$')
_GIT = shutil.which('git') or 'git'


class SandboxError(RuntimeError):
  """A sandbox workspace could not be created or removed."""


@dataclasses.dataclass(frozen=True)
class Sandbox:
  """The microVM that runs a sandbox workspace's tools.

  Attributes:
    id: Names the VM; a worker started with the same ID reattaches to it.
    image: The OCI image the VM boots.
    egress: Hosts the VM may reach over HTTPS.
    cpus: Virtual CPUs.
    memory_mib: Memory in MiB.
  """

  id: str
  image: str = DEFAULT_IMAGE
  egress: tuple[str, ...] = DEFAULT_EGRESS
  cpus: int = DEFAULT_CPUS
  memory_mib: int = DEFAULT_MEMORY_MIB

  def worker_config(self, sandboxd: pathlib.Path) -> dict[str, object]:
    """The worker's `sandbox` configuration, mounting `sandboxd` in the VM."""
    return {
      'id': self.id,
      'image': self.image,
      'sandboxd': str(sandboxd),
      'egress': list(self.egress),
      'cpus': self.cpus,
      'memory_mib': self.memory_mib,
    }

  def to_json(self) -> str:
    """The form `from_json` reads back."""
    return json.dumps(dataclasses.asdict(self))

  @classmethod
  def from_json(cls, text: str) -> Sandbox:
    """Read a stored sandbox, rejecting anything this version cannot run."""
    value = cast(object, json.loads(text))
    if not isinstance(value, dict):
      raise SandboxError(f'stored sandbox is not an object: {text!r}')
    values = cast(dict[str, object], value)
    return _sandbox(str(values.get('id', '')), values)

  @classmethod
  def from_settings(cls, settings: Mapping[str, object]) -> Sandbox:
    """A new sandbox configured by the `sandbox` settings object."""
    configured = settings.get('sandbox', {})
    if not isinstance(configured, dict):
      raise SandboxError('the "sandbox" setting must be an object')
    return _sandbox(str(uuid.uuid4()), cast(dict[str, object], configured))


def _sandbox(sandbox_id: str, values: Mapping[str, object]) -> Sandbox:
  image = values.get('image', DEFAULT_IMAGE)
  egress = values.get('egress', list(DEFAULT_EGRESS))
  cpus = values.get('cpus', DEFAULT_CPUS)
  memory_mib = values.get('memory_mib', DEFAULT_MEMORY_MIB)
  try:
    uuid.UUID(sandbox_id)
  except ValueError as error:
    raise SandboxError(f'sandbox id {sandbox_id!r} is not a UUID') from error
  if not isinstance(image, str) or not image:
    raise SandboxError('sandbox "image" must be a non-empty string')
  hosts = cast(list[object], egress) if isinstance(egress, list) else None
  if hosts is None or not all(isinstance(host, str) for host in hosts):
    raise SandboxError('sandbox "egress" must be a list of host names')
  if (
    not isinstance(cpus, int)
    or isinstance(cpus, bool)
    or not 1 <= cpus <= _MAX_CPUS
  ):
    raise SandboxError(f'sandbox "cpus" must be from 1 to {_MAX_CPUS}')
  if (
    not isinstance(memory_mib, int)
    or isinstance(memory_mib, bool)
    or not 256 <= memory_mib <= _MAX_MEMORY_MIB
  ):
    raise SandboxError(
      f'sandbox "memory_mib" must be from 256 to {_MAX_MEMORY_MIB}'
    )
  return Sandbox(
    sandbox_id, image, tuple(str(host) for host in hosts), cpus, memory_mib
  )


def default_root() -> pathlib.Path:
  """Where sandbox clones live."""
  return pathlib.Path.home() / '.mirror' / 'sandboxes'


def sandboxd_path() -> pathlib.Path:
  """The Linux `sandboxd` to mount: `$MIRROR_SANDBOXD`, or this install's.

  The file may be missing, as in a development checkout; the VM then fails to
  start rather than its tools running unsandboxed.
  """
  override = os.environ.get('MIRROR_SANDBOXD')
  if override:
    return pathlib.Path(override).absolute()
  return pathlib.Path(__file__).parents[1] / 'bin' / 'linux' / 'mirror-sandboxd'


def create_default_clone(
  source: pathlib.Path | None, name: str | None
) -> tuple[pathlib.Path, str]:
  """`create_clone` under the user's sandbox directory."""
  return create_clone(source, name, root=default_root())


def create_clone(
  source: pathlib.Path | None, name: str | None, *, root: pathlib.Path
) -> tuple[pathlib.Path, str]:
  """Create an empty private directory or clone a host's committed revision.

  Returns:
    The clone's resolved path and the sandbox's name.
  """
  name = _sandbox_name(name)
  path = root / name
  if path.exists():
    raise SandboxError(f'sandbox {name!r} already exists at {path}')
  root.mkdir(parents=True, exist_ok=True, mode=0o700)
  if source is None:
    path.mkdir(mode=0o700)
    return path.resolve(), name
  store, revision = worktrees.local_revision(source)
  # Copied objects prevent guest writes from reaching the source repository.
  _git(
    root,
    'clone',
    '--quiet',
    '--no-hardlinks',
    '--no-checkout',
    str(store),
    str(path),
  )
  try:
    _git(path, 'switch', '--quiet', '--detach', revision)
    _configure_clone(source, path, name)
  except SandboxError:
    shutil.rmtree(path, ignore_errors=True)
    raise
  return path.resolve(), name


def _configure_clone(
  repository: pathlib.Path, path: pathlib.Path, name: str
) -> None:
  _git(path, 'switch', '--quiet', '-c', f'mirror/{name}')
  # The guest cannot reach the source path, so `origin` names the source's own
  # upstream when it has one, without any credentials embedded in it.
  upstream = _git_value(repository, 'remote.origin.url')
  if upstream is not None:
    _git(path, 'remote', 'set-url', 'origin', _without_credentials(upstream))
  for key in ('user.name', 'user.email'):
    value = _git_value(repository, key)
    if value is not None:
      _git(path, 'config', key, value)


def _without_credentials(url: str) -> str:
  """`url` without the credentials it embeds, which the guest could read.

  The query and fragment go, since Git needs neither and either can carry a
  token. HTTP user names are often tokens; other schemes keep theirs, such as
  SSH's `git`, and lose only a password.
  """
  parts = urllib.parse.urlsplit(url)
  if not parts.scheme:
    return url
  userinfo, at, host = parts.netloc.rpartition('@')
  user = userinfo.partition(':')[0]
  keep = f'{user}@' if at and parts.scheme not in ('http', 'https') else ''
  return urllib.parse.urlunsplit(
    parts._replace(netloc=keep + host, query='', fragment='')
  )


def remove_clone(path: pathlib.Path) -> None:
  """Delete a sandbox clone; one that is already gone is fine."""
  with contextlib.suppress(FileNotFoundError):
    shutil.rmtree(path)


def _sandbox_name(name: str | None) -> str:
  normalized = (name or '').strip().lower()
  if not normalized:
    return f'sandbox-{uuid.uuid4().hex[:6]}'
  if not _SAFE_NAME.fullmatch(normalized):
    raise SandboxError(
      'sandbox name must be 1-40 lowercase letters, digits or dashes'
    )
  return normalized


def _git(cwd: pathlib.Path, *arguments: str) -> str:
  completed = subprocess.run(
    (_GIT, '-C', str(cwd), *arguments),
    check=False,
    capture_output=True,
    text=True,
    close_fds=False,
  )
  if completed.returncode != 0:
    detail = completed.stderr.strip() or completed.stdout.strip()
    raise SandboxError(detail or f'git {arguments[0]} failed')
  return completed.stdout.strip()


def _git_value(repository: pathlib.Path, key: str) -> str | None:
  """A configuration value, or `None` when it is unset."""
  try:
    return _git(repository, 'config', '--get', key) or None
  except SandboxError:
    return None
