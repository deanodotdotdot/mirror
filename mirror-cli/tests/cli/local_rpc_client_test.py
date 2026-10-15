"""Worker diagnostics and startup events stay lossless across async delivery."""

import asyncio
import base64
import pathlib
import sys

import pytest
from mirror.protocol import agent_pb2

from mirror.cli import local_rpc_client


@pytest.mark.asyncio
async def test_transport_preserves_long_diagnostics_and_awaits_startup_events(
  tmp_path: pathlib.Path,
) -> None:
  event = agent_pb2.SessionMessage(
    replay_done=agent_pb2.ReplayDone(conversation_id='synthetic')
  )
  encoded = base64.b64encode(event.SerializeToString())
  source = f"""
import asyncio
import sys
import grpc.aio

async def main():
  server = grpc.aio.server()
  port = server.add_insecure_port('127.0.0.1:0')
  await server.start()
  sys.stderr.buffer.write(b'x' * (1 << 18))
  sys.stderr.buffer.flush()
  sys.stdout.buffer.write(b'MIRROR_SESSION_MESSAGE=' + {encoded!r} + b'\\n')
  print('before address', flush=True)
  print(f'MIRROR_RPC_ADDRESS=127.0.0.1:{{port}}', flush=True)
  print('after address', flush=True)
  await server.wait_for_termination()

asyncio.run(main())
"""
  received: list[agent_pb2.SessionMessage] = []

  async def receive(message: agent_pb2.SessionMessage) -> None:
    await asyncio.sleep(0)
    received.append(message)

  transport = local_rpc_client.Transport(
    [sys.executable, '-c', source],
    {},
    workspace=tmp_path,
    log_dir=tmp_path,
    on_message=receive,
  )
  try:
    await asyncio.wait_for(transport.connect(), timeout=5)
    assert received == [event]
  finally:
    await transport.close()
  assert (tmp_path / 'stderr').stat().st_mode & 0o777 == 0o600
  assert (tmp_path / 'stderr').read_bytes() == b'x' * (1 << 18)
  assert (
    tmp_path / 'stdout'
  ).read_bytes() == b'before address\nafter address\n'
