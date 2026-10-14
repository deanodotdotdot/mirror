"""Pinned, consent-gated sandbox downloads; no installers, sudo or host credentials."""

from __future__ import annotations

import asyncio
import dataclasses
import hashlib
import io
import json
import pathlib
import platform
import os
import re
import shutil
from collections.abc import Mapping, Sequence
from typing import cast
import tarfile
import tempfile

import httpx2

from mirror import config
from mirror.cli import shell


@dataclasses.dataclass(frozen=True)
class Artifact:
  """One immutable upstream download with an exact byte limit."""

  url: str
  sha256: str
  size: int


# The runtime CLI/streaming contract is pinned; update these from release checksums.
_RUNTIME = {
  'darwin-aarch64': (
    Artifact(
      'https://github.com/superradcompany/microsandbox/releases/download/v0.7.7/msb-darwin-aarch64',
      '2dc6b445fe7423f7393984398cf82748c7628298c3a8aa2fb2d2c2c7bd2e0ae8',
      36871952,
    ),
    Artifact(
      'https://github.com/superradcompany/microsandbox/releases/download/v0.7.7/libkrunfw-darwin-aarch64.dylib',
      '8200762c585f181e7285d65b5af257c4d4493ebdef0be5cf4b65f475e87c4a21',
      24801440,
    ),
  ),
  'linux-aarch64': (
    Artifact(
      'https://github.com/superradcompany/microsandbox/releases/download/v0.7.7/msb-linux-aarch64',
      'e973562576f552874657e4de911e5460aee2a2ef19a717c2eec50b6489b59964',
      40030912,
    ),
    Artifact(
      'https://github.com/superradcompany/microsandbox/releases/download/v0.7.7/libkrunfw-linux-aarch64.so',
      'a600787bec77b5549a08c9750f9ec0594cbbfa7b31679ff6defbbb2bc1d5d216',
      24840176,
    ),
  ),
  'linux-x86_64': (
    Artifact(
      'https://github.com/superradcompany/microsandbox/releases/download/v0.7.7/msb-linux-x86_64',
      '77326170666e686f66cbdbb6db853ed2860bc4c8c90b54c2c41e8c610b87b8f6',
      48513720,
    ),
    Artifact(
      'https://github.com/superradcompany/microsandbox/releases/download/v0.7.7/libkrunfw-linux-x86_64.so',
      'ce0a915daa6aff403e9a388e161671fbac9b85b0c200a99d7a9fdf0e1c03d668',
      21628880,
    ),
  ),
}

# Official Python bookworm includes Bash, Python, Git and GNU tar. Ripgrep is seeded separately.
_IMAGES = {
  'aarch64': (
    'docker.io/library/python@sha256:715a063709d47191add11cccf48b4de0cdf220302070ca78d10c249f721f1da6',
    373102771,
  ),
  'x86_64': (
    'docker.io/library/python@sha256:edd0b3ec946cc68bd20e39480bd03929016d0fb6254cbf870cd3bc55ea25116d',
    382717142,
  ),
}
_RG = {
  'aarch64': Artifact(
    'https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-aarch64-unknown-linux-gnu.tar.gz',
    'c827481c4ff4ea10c9dc7a4022c8de5db34a5737cb74484d62eb94a95841ab2f',
    2047405,
  ),
  'x86_64': Artifact(
    'https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-musl.tar.gz',
    '4cf9f2741e6c465ffdb7c26f38056a59e2a2544b51f7cc128ef28337eeae4d8e',
    2566310,
  ),
}


def architecture() -> str:
  """Return the upstream architecture spelling."""
  machine = platform.machine()
  if machine in ('arm64', 'aarch64'):
    return 'aarch64'
  if machine == 'x86_64':
    return machine
  raise RuntimeError(
    'Sandbox requires Apple Silicon macOS or arm64/x86_64 Linux.'
  )


def runtime_artifacts() -> tuple[Artifact, Artifact]:
  """Select a version-pinned executable and its matching kernel library."""
  pair = _RUNTIME.get(f'{platform.system().lower()}-{architecture()}')
  if pair is None:
    raise RuntimeError(
      'Sandbox requires Apple Silicon macOS or arm64/x86_64 Linux.'
    )
  return pair


def directory() -> pathlib.Path:
  """Private assets and cache shared only by Mirror sandboxes."""
  return pathlib.Path.home() / '.mirror' / 'sandbox-assets'


def runtime_directory() -> pathlib.Path:
  """An installation distinct from system and user Microsandbox installations."""
  return (
    directory()
    / f'microsandbox-0.7.7-{platform.system().lower()}-{architecture()}'
  )


def default_image() -> tuple[str, int]:
  """Return the native official image and compressed download size."""
  return _IMAGES[architecture()]


def image_marker(image: str) -> pathlib.Path:
  """Completion marker scoped to the exact approved image reference."""
  return directory() / (hashlib.sha256(image.encode()).hexdigest() + '.ready')


def rg_path() -> pathlib.Path:
  """The pinned guest-tool archive, never executed on the host."""
  return directory() / _RG[architecture()].url.rsplit('/', 1)[-1]


def verified_bytes(path: pathlib.Path, artifact: Artifact) -> bytes:
  """Bound reads and reject corrupted or substituted downloads."""
  with path.open('rb') as source:
    data = source.read(artifact.size + 1)
  if (
    len(data) != artifact.size
    or hashlib.sha256(data).hexdigest() != artifact.sha256
  ):
    raise RuntimeError(
      f'Checksum verification failed for {path.name}; remove this file and retry.'
    )
  return data


async def download(artifact: Artifact, destination: pathlib.Path) -> None:
  """Download atomically, without ambient proxies, netrc or authentication."""
  destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
  if destination.exists():
    verified_bytes(destination, artifact)
    return
  with tempfile.TemporaryDirectory(
    dir=destination.parent, prefix='.download-'
  ) as temporary:
    path = pathlib.Path(temporary) / 'payload'
    digest, size = hashlib.sha256(), 0
    try:
      async with (
        asyncio.timeout(180),
        httpx2.AsyncClient(trust_env=False, timeout=30) as client,
      ):
        url = artifact.url
        for _ in range(5):
          parsed = httpx2.URL(url)
          if parsed.scheme != 'https' or parsed.host not in (
            'github.com',
            'release-assets.githubusercontent.com',
            'objects.githubusercontent.com',
          ):
            raise RuntimeError('Refusing an unexpected download redirect.')
          async with client.stream('GET', url) as response:
            if response.is_redirect:
              location = response.headers.get('location')
              if not location:
                raise RuntimeError(
                  'Download server returned an incomplete redirect.'
                )
              url = str(response.url.join(location))
              continue
            response.raise_for_status()
            with path.open('wb') as output:
              async for chunk in response.aiter_bytes():
                size += len(chunk)
                if size > artifact.size:
                  raise RuntimeError('Download exceeded its expected size.')
                digest.update(chunk)
                output.write(chunk)
            break
        else:
          raise RuntimeError('Too many download redirects.')
    except (httpx2.HTTPError, TimeoutError):
      raise RuntimeError(
        f'Download failed for {destination.name}. Check your connection and retry.'
      ) from None
    if size != artifact.size or digest.hexdigest() != artifact.sha256:
      raise RuntimeError(
        f'Checksum verification failed for {destination.name}. Nothing was installed.'
      )
    path.chmod(0o600)
    path.replace(destination)


async def install_runtime() -> str:
  """Install only the approved pinned files, without executing an installer."""
  executable, library = runtime_artifacts()
  root = runtime_directory()
  root.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
  library_name = (
    'libkrunfw.dylib' if platform.system() == 'Darwin' else 'libkrunfw.so'
  )
  if root.exists():
    verified_bytes(root / 'msb', executable)
    verified_bytes(root / library_name, library)
    return str(root / 'msb')
  with tempfile.TemporaryDirectory(
    dir=root.parent, prefix='.runtime-'
  ) as temporary:
    staging = pathlib.Path(temporary)
    await download(executable, staging / 'msb')
    await download(library, staging / library_name)
    (staging / 'msb').chmod(0o700)
    try:
      staging.rename(root)
    except OSError:
      if not root.is_dir():
        raise
      verified_bytes(root / 'msb', executable)
      verified_bytes(root / library_name, library)
  return str(root / 'msb')


async def prepare_guest_tools() -> None:
  """Fetch the approved guest-only ripgrep archive."""
  await download(_RG[architecture()], rg_path())


def guest_rg() -> bytes:
  """Read only the regular rg member from the verified upstream archive."""
  artifact = _RG[architecture()]
  data = verified_bytes(rg_path(), artifact)
  member = artifact.url.rsplit('/', 1)[-1].removesuffix('.tar.gz') + '/rg'
  with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
    info = archive.getmember(member)
    if not info.isfile() or info.size > 16 * 1024 * 1024:
      raise RuntimeError('Invalid guest ripgrep archive.')
    source = archive.extractfile(info)
    if source is None:
      raise RuntimeError('Missing guest ripgrep binary.')
    with source:
      return source.read(16 * 1024 * 1024 + 1)


@dataclasses.dataclass
class Options:
  """Explicit, non-secret host settings for new sandboxes."""

  executable: str = ''
  image: str = ''

  @classmethod
  def load(cls, settings: Mapping[str, object] | None = None) -> Options:
    """Read saved choices, retaining explicit runtime and image overrides."""
    configured = (
      config.Config(config.default_path()).read()
      if settings is None
      else settings
    ).get('sandbox', {})
    if not isinstance(configured, dict):
      raise ValueError('sandbox settings must be a JSON object')
    values = cast(dict[str, object], configured)
    managed = runtime_directory() / 'msb'
    executable = os.environ.get('MIRROR_MSB') or values.get(
      'executable',
      str(managed) if managed.is_file() else shutil.which('msb') or '',
    )
    image = os.environ.get(
      'MIRROR_SANDBOX_IMAGE',
      values.get('image', default_image()[0]),
    )
    if not isinstance(executable, str) or not isinstance(image, str):
      raise ValueError('sandbox executable and image must be strings')
    return cls(executable, image)

  def save(self, settings: config.Config) -> None:
    """Remember choices without changing resource or egress preferences."""

    def update(values: dict[str, object]) -> None:
      previous = values.get('sandbox', {})
      if not isinstance(previous, dict):
        raise ValueError('sandbox settings must be a JSON object')
      values['sandbox'] = cast(
        dict[str, object], previous
      ) | dataclasses.asdict(self)

    settings.update(update)

  def runtime_ready(self) -> bool:
    """Whether an executable and matching library are present."""
    try:
      return (
        bool(self.executable)
        and self.binary_path().is_file()
        and os.access(self.binary_path(), os.X_OK)
        and self.library_path() is not None
      )
    except (OSError, ValueError, RuntimeError):
      return False

  def image_ready(self) -> bool:
    """Whether this exact image has completed an approved download."""
    return image_marker(self.image).is_file() and (
      self.image != default_image()[0] or rg_path().is_file()
    )

  def binary_path(self) -> pathlib.Path:
    """Resolve the explicitly chosen host executable."""
    return pathlib.Path(self.executable).expanduser().resolve()

  def library_path(self) -> pathlib.Path | None:
    """Find the library beside the executable or in the installation lib directory."""
    binary = self.binary_path()
    name = (
      'libkrunfw.dylib' if platform.system() == 'Darwin' else 'libkrunfw.so'
    )
    override = os.environ.get('MSB_LIBKRUNFW_PATH')
    if override:
      path = pathlib.Path(override).expanduser().resolve()
      return path if path.is_file() else None
    return next(
      (
        path
        for path in (
          binary.parent / name,
          binary.parent.parent / 'lib' / name,
        )
        if path.is_file()
      ),
      None,
    )

  def checks(self) -> list[tuple[str, bool]]:
    """Describe each prerequisite independently without launching a process."""
    try:
      executable_ready = (
        bool(self.executable)
        and self.binary_path().is_file()
        and os.access(self.binary_path(), os.X_OK)
      )
      library_ready = bool(self.executable) and self.library_path() is not None
    except (OSError, ValueError, RuntimeError):
      executable_ready = library_ready = False
    return [
      *host_checks(),
      (
        'Runtime: Microsandbox 0.7.7 executable (version checked on start)',
        executable_ready,
      ),
      (
        'Runtime library: matching libkrunfw beside msb or in ../lib',
        library_ready,
      ),
      (
        'Image: public OCI reference pinned with @sha256:<64 hex digits>',
        re.fullmatch(
          r'[a-z0-9][a-z0-9._:-]*(?:/[a-z0-9][a-z0-9._-]*)*@sha256:[0-9a-f]{64}',
          self.image,
        )
        is not None,
      ),
    ]


def runtime_environment(options: Options) -> dict[str, str]:
  """Keep Microsandbox state and registry authentication outside the host home."""
  library = options.library_path()
  if library is None:
    raise RuntimeError(
      'Missing matching libkrunfw; install the supported runtime.'
    )
  root = pathlib.Path.home() / '.mirror' / 'msb'
  root.mkdir(parents=True, exist_ok=True, mode=0o700)
  cache = directory() / 'cache'
  cache.mkdir(parents=True, exist_ok=True, mode=0o700)
  configuration = root / 'config.json'
  with tempfile.TemporaryDirectory(dir=root) as temporary:
    staged = pathlib.Path(temporary) / 'config.json'
    staged.write_text(json.dumps({'paths': {'cache': str(cache)}}))
    staged.replace(configuration)
  return {
    'PATH': '/usr/bin:/bin',
    'HOME': str(root),
    'MIRROR_MSB': str(options.binary_path()),
    'MSB_HOME': str(root),
    'MSB_CONFIG_PATH': str(configuration),
    'MSB_LIBKRUNFW_PATH': str(library),
  }


def worker_command(command: Sequence[str]) -> tuple[str, ...]:
  """Configure only the worker's native VM client; host identity stays unchanged."""
  environment = runtime_environment(Options.load())
  return (
    '/usr/bin/env',
    *(
      f'{key}={environment[key]}'
      for key in (
        'MIRROR_MSB',
        'MSB_HOME',
        'MSB_CONFIG_PATH',
        'MSB_LIBKRUNFW_PATH',
      )
    ),
    *command,
  )


async def prepare_image(options: Options) -> None:
  """Pull the approved image and guest tools without forwarding registry secrets."""
  missing = [label for label, ready in options.checks() if not ready]
  if missing:
    raise RuntimeError('\n'.join(missing))
  environment = runtime_environment(options)
  launcher = shell.Shell(
    pathlib.Path(environment['HOME']), environment=environment
  )
  version = await launcher.run((environment['MIRROR_MSB'], '--version'))
  if version.returncode or version.stdout.strip() not in (
    b'msb 0.7.7',
    b'microsandbox 0.7.7',
  ):
    raise RuntimeError('Microsandbox 0.7.7 is required for this setup flow.')
  if options.image == default_image()[0]:
    await prepare_guest_tools()
  result = await launcher.run(
    (environment['MIRROR_MSB'], 'image', 'pull', options.image, '--quiet'),
    timeout=600,
    limit=65536,
  )
  if result.returncode:
    raise RuntimeError(
      'Image download failed. Check anonymous registry access and retry.'
    )
  image_marker(options.image).touch(mode=0o600)


def host_checks() -> list[tuple[str, bool]]:
  """Host constraints that installing downloads cannot repair."""
  system, machine = platform.system(), platform.machine()
  managed = pathlib.Path(
    '/Library/Application Support/microsandbox/managed.json'
    if system == 'Darwin'
    else '/etc/microsandbox/managed.json'
  )
  return [
    (
      'Platform: Apple Silicon macOS or arm64/x86_64 Linux',
      (system, machine)
      in {('Darwin', 'arm64'), ('Linux', 'aarch64'), ('Linux', 'x86_64')},
    ),
    (
      'Virtualization: Linux requires read/write access to /dev/kvm',
      system != 'Linux' or os.access('/dev/kvm', os.R_OK | os.W_OK),
    ),
    (
      'Isolation: no machine-wide Microsandbox managed configuration',
      not managed.exists(),
    ),
  ]
