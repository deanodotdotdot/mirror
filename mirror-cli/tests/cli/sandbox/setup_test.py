import asyncio
import functools
import hashlib
import io
import pathlib
import tarfile
from collections.abc import AsyncIterator
from typing import override

import httpx2
import pytest

from mirror.cli.sandbox import setup as sandbox_setup


@pytest.fixture(autouse=True)
def isolated_home(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.setenv('HOME', str(tmp_path))
  monkeypatch.setattr(sandbox_setup.platform, 'system', lambda: 'Darwin')
  monkeypatch.setattr(sandbox_setup.platform, 'machine', lambda: 'arm64')


def artifact(data: bytes, name: str = 'binary') -> sandbox_setup.Artifact:
  return sandbox_setup.Artifact(
    'https://github.com/example/' + name,
    hashlib.sha256(data).hexdigest(),
    len(data),
  )


@pytest.mark.asyncio
async def test_download_is_verified_atomic_and_does_not_use_host_auth(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  monkeypatch.setenv('HTTPS_PROXY', 'http://synthetic-secret@proxy.invalid')
  monkeypatch.setenv('NETRC', str(tmp_path / 'netrc'))
  (tmp_path / 'netrc').write_text(
    'machine github.com login synthetic password synthetic-secret\n'
  )
  requests: list[httpx2.Request] = []

  def respond(request: httpx2.Request) -> httpx2.Response:
    requests.append(request)
    assert 'authorization' not in request.headers
    assert 'proxy-authorization' not in request.headers
    return httpx2.Response(200, content=b'approved bytes')

  monkeypatch.setattr(
    httpx2,
    'AsyncClient',
    functools.partial(
      httpx2.AsyncClient, transport=httpx2.MockTransport(respond)
    ),
  )
  target = tmp_path / 'downloads' / 'msb'
  await sandbox_setup.download(artifact(b'approved bytes'), target)
  assert target.read_bytes() == b'approved bytes'
  assert target.stat().st_mode & 0o777 == 0o600
  assert len(requests) == 1
  assert list(target.parent.iterdir()) == [target]
  await sandbox_setup.download(artifact(b'approved bytes'), target)
  assert len(requests) == 1, (
    'Verified downloads are reused without network requests'
  )


@pytest.mark.asyncio
@pytest.mark.parametrize(
  'content', [b'tampered', b'too large to be the approved asset', b'']
)
async def test_bad_download_never_becomes_an_installation(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch, content: bytes
) -> None:
  def respond(request: httpx2.Request) -> httpx2.Response:
    del request
    return httpx2.Response(200, content=content)

  monkeypatch.setattr(
    httpx2,
    'AsyncClient',
    functools.partial(
      httpx2.AsyncClient, transport=httpx2.MockTransport(respond)
    ),
  )
  target = tmp_path / 'msb'
  with pytest.raises(RuntimeError):
    await sandbox_setup.download(artifact(b'original'), target)
  assert not target.exists()
  assert not list(tmp_path.glob('.download-*'))


@pytest.mark.asyncio
async def test_redirect_cannot_send_downloads_to_an_arbitrary_host(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  requests: list[str] = []

  def respond(request: httpx2.Request) -> httpx2.Response:
    requests.append(str(request.url))
    return httpx2.Response(
      302, headers={'location': 'http://localhost/private'}
    )

  monkeypatch.setattr(
    httpx2,
    'AsyncClient',
    functools.partial(
      httpx2.AsyncClient, transport=httpx2.MockTransport(respond)
    ),
  )
  with pytest.raises(RuntimeError, match='unexpected download redirect'):
    await sandbox_setup.download(artifact(b'original'), tmp_path / 'msb')
  assert requests == ['https://github.com/example/binary']


class _BlockedStream(httpx2.AsyncByteStream):
  def __init__(self, started: asyncio.Event) -> None:
    self.started = started

  @override
  async def __aiter__(self) -> AsyncIterator[bytes]:
    self.started.set()
    await asyncio.Event().wait()
    yield b'never completed'


@pytest.mark.asyncio
async def test_cancelling_download_removes_partial_files(
  tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
  started = asyncio.Event()

  def respond(request: httpx2.Request) -> httpx2.Response:
    del request
    return httpx2.Response(200, stream=_BlockedStream(started))

  monkeypatch.setattr(
    httpx2,
    'AsyncClient',
    functools.partial(
      httpx2.AsyncClient, transport=httpx2.MockTransport(respond)
    ),
  )
  task = asyncio.create_task(
    sandbox_setup.download(artifact(b'original'), tmp_path / 'msb')
  )
  await asyncio.wait_for(started.wait(), 3)
  task.cancel()
  with pytest.raises(asyncio.CancelledError):
    await task
  assert not (tmp_path / 'msb').exists()
  assert not list(tmp_path.glob('.download-*'))


@pytest.mark.asyncio
async def test_install_publishes_only_a_complete_verified_pair(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.setattr(
    sandbox_setup,
    'runtime_artifacts',
    lambda: (artifact(b'executable', 'msb'), artifact(b'library', 'library')),
  )

  def respond(request: httpx2.Request) -> httpx2.Response:
    return httpx2.Response(
      200,
      content=b'executable'
      if request.url.path.endswith('/msb')
      else b'library',
    )

  monkeypatch.setattr(
    httpx2,
    'AsyncClient',
    functools.partial(
      httpx2.AsyncClient, transport=httpx2.MockTransport(respond)
    ),
  )
  executable = pathlib.Path(await sandbox_setup.install_runtime())
  assert executable.read_bytes() == b'executable'
  assert executable.stat().st_mode & 0o777 == 0o700
  assert (executable.parent / 'libkrunfw.dylib').read_bytes() == b'library'
  assert not list(executable.parent.parent.glob('.runtime-*'))


@pytest.mark.asyncio
async def test_partial_runtime_is_not_published(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  monkeypatch.setattr(
    sandbox_setup,
    'runtime_artifacts',
    lambda: (artifact(b'executable', 'msb'), artifact(b'library', 'library')),
  )

  def respond(request: httpx2.Request) -> httpx2.Response:
    return httpx2.Response(
      200,
      content=b'executable'
      if request.url.path.endswith('/msb')
      else b'corrupt',
    )

  monkeypatch.setattr(
    httpx2,
    'AsyncClient',
    functools.partial(
      httpx2.AsyncClient, transport=httpx2.MockTransport(respond)
    ),
  )
  with pytest.raises(RuntimeError, match='Checksum'):
    await sandbox_setup.install_runtime()
  assert not sandbox_setup.runtime_directory().exists()


def test_guest_tool_is_read_without_extracting_other_archive_paths(
  monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path
) -> None:
  archive = io.BytesIO()
  with tarfile.open(fileobj=archive, mode='w:gz') as output:
    rg = tarfile.TarInfo('binary/rg')
    rg.size = 2
    output.addfile(rg, io.BytesIO(b'rg'))
    malicious = tarfile.TarInfo('../../outside')
    malicious.size = 4
    output.addfile(malicious, io.BytesIO(b'evil'))
  data = archive.getvalue()
  descriptor = artifact(data, 'binary.tar.gz')
  monkeypatch.setattr(sandbox_setup, '_RG', {'aarch64': descriptor})
  path = sandbox_setup.rg_path()
  path.parent.mkdir(parents=True)
  path.write_bytes(data)
  assert sandbox_setup.guest_rg() == b'rg'
  assert not (tmp_path / '.mirror' / 'outside').exists()


@pytest.mark.asyncio
async def test_concurrent_approved_installs_share_one_complete_runtime(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  executable, library = (
    artifact(b'executable', 'msb'),
    artifact(b'library', 'library'),
  )
  monkeypatch.setattr(
    sandbox_setup, 'runtime_artifacts', lambda: (executable, library)
  )

  async def download(
    descriptor: sandbox_setup.Artifact, destination: pathlib.Path
  ) -> None:
    destination.write_bytes(
      b'executable' if descriptor == executable else b'library'
    )
    await asyncio.sleep(0)

  monkeypatch.setattr(sandbox_setup, 'download', download)
  first, second = await asyncio.gather(
    sandbox_setup.install_runtime(), sandbox_setup.install_runtime()
  )
  assert first == second
  assert pathlib.Path(first).read_bytes() == b'executable'
  assert not list(sandbox_setup.directory().glob('.runtime-*'))
