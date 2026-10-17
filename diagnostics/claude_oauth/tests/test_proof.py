"""Synthetic only: these tests never contact Anthropic or read existing logins."""

import contextlib
import copy
import io
import json
import os
from pathlib import Path
import pty
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
import urllib.parse
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import compatibility as compat
import proof


def setUpModule():
    # A newly introduced unmocked HTTP/browser path must fail before it can
    # contact a provider or launch a browser. Raw loopback callback tests remain
    # available; explicit instance mocks still exercise transport behavior.
    global network_guard, browser_guard
    network_guard = patch.object(urllib.request.OpenerDirector, "open", side_effect=AssertionError("Synthetic tests forbid live HTTP"))
    browser_guard = patch.object(proof.webbrowser, "open", side_effect=AssertionError("Synthetic tests forbid browser launch"))
    network_guard.start()
    browser_guard.start()


def tearDownModule():
    browser_guard.stop()
    network_guard.stop()


ACCOUNT = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
ORGANIZATION = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
INSTALLATION = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
SESSION = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
ACCESS = "sk-ant-oat01-SYNTHETIC_ACCESS_CANARY"
REFRESH = "sk-ant-ort01-SYNTHETIC_REFRESH_CANARY"
TOKEN = {"access_token": ACCESS, "refresh_token": REFRESH, "token_type": "Bearer",
         "expires_in": 3600, "scope": "user:profile user:inference",
         "account": {"uuid": ACCOUNT}, "organization": {"uuid": ORGANIZATION}}


def grant():
    return proof.Grant.from_token(copy.deepcopy(TOKEN))


def record():
    return {"schema": proof.STORE_SCHEMA, "generation": str(proof.uuid.uuid4()), "revision": 1,
            "installation_id": INSTALLATION, "credential": grant().private_record()}


def sse(events):
    return b"".join(b"event: " + event["type"].encode() + b"\ndata: " + compat.json_bytes(event) + b"\n\n" for event in events)


def response_events(blocks, reason="end_turn"):
    result = [{"type": "message_start", "message": {"id": "msg_synthetic"}}]
    for index, block in enumerate(blocks):
        result += [{"type": "content_block_start", "index": index, "content_block": block},
                   {"type": "content_block_stop", "index": index}]
    return result + [{"type": "message_delta", "delta": {"stop_reason": reason}}, {"type": "message_stop"}]


class FakeTransport:
    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    def json(self, operation, token=None, data=None, **kwargs):
        self.calls.append((operation, token, data, kwargs))
        value = self.responses.pop(0)
        if isinstance(value, Exception):
            raise value
        return copy.deepcopy(value)

    @contextlib.contextmanager
    def response(self, operation, token=None, body=None, **kwargs):
        self.calls.append((operation, token, body, kwargs))
        yield io.BytesIO(self.responses.pop(0))


class TokenTests(unittest.TestCase):
    def test_secret_repr_and_error_are_redacted(self):
        value = grant()
        output = repr(value) + repr(value.access) + str(value.refresh)
        self.assertNotIn(ACCESS, output)
        self.assertNotIn(REFRESH, output)
        self.assertNotIn(ACCESS, str(compat.ProofError("invalid_token_response")))

    def test_malformed_and_wrong_kind_tokens(self):
        bad_fields = {"access_token": ("", "sk-ant-api01-KEY", None), "refresh_token": ("", None),
                      "token_type": ("mac", None), "expires_in": (0, -1, True, "3600", None),
                      "scope": ("user:profile", [], None)}
        for key, values in bad_fields.items():
            for value in values:
                with self.subTest(key=key, value=value), self.assertRaises(compat.ProofError):
                    proof.Grant.from_token({**TOKEN, key: value})

    def test_refresh_preserves_omitted_refresh_and_identity(self):
        previous = grant()
        data = {"access_token": ACCESS + "2", "token_type": "bearer", "expires_in": 1200}
        current = proof.Grant.from_token(data, previous, now=1000)
        self.assertEqual(current.refresh.reveal(), REFRESH)
        self.assertEqual(current.expires_at, 2200)
        self.assertEqual(current.authorized_at, previous.authorized_at)
        self.assertEqual(current.organization_id, ORGANIZATION)
        for field in ("account", "organization"):
            with self.assertRaisesRegex(compat.ProofError, "account_changed"):
                proof.Grant.from_token({**data, field: {"uuid": INSTALLATION}}, previous)


class CallbackTests(unittest.TestCase):
    def setUp(self):
        self.pending = proof.PendingLogin("http://localhost:54545/callback")

    def url(self, code="SYNTHETIC_CODE", state=None):
        return self.pending.redirect_uri + "?" + urllib.parse.urlencode({
            "code": code, "state": state if state is not None else self.pending.state.reveal()})

    def test_pkce_and_exact_redirect(self):
        params = urllib.parse.parse_qs(urllib.parse.urlsplit(self.pending.authorization_url()).query)
        expected = proof.base64.urlsafe_b64encode(proof.hashlib.sha256(self.pending.verifier.reveal().encode()).digest()).decode().rstrip("=")
        self.assertEqual(params["code_challenge"], [expected])
        self.assertEqual(params["redirect_uri"], [self.pending.redirect_uri])
        self.assertEqual(params["scope"], [" ".join(compat.SCOPES)])
        code = self.pending.parse(self.url())
        body = self.pending.claim(code)
        self.assertEqual(body["redirect_uri"], params["redirect_uri"][0])
        self.assertEqual(body["state"], self.pending.state.reveal())
        self.assertEqual(body["code_verifier"], self.pending.verifier.reveal())
        with self.assertRaisesRegex(compat.ProofError, "callback_replayed"):
            self.pending.parse(self.url())

    def test_every_supplied_state_is_validated(self):
        valid = self.pending.state.reveal()
        values = [self.url(state="wrong"), self.url(code="code#wrong"),
                  self.url(code="code#"), self.url() + "&state=" + valid,
                  self.url() + "&code=second", self.url().replace("localhost", "evil.test"),
                  self.url().replace("/callback", "/other"), "sk-ant-oat01-DO_NOT_IMPORT",
                  self.pending.redirect_uri + "?code=code", "code#wrong", "code#π", self.url(state="π"), "x" * 8193]
        for value in values:
            with self.subTest(value=value[:30]), self.assertRaises(compat.ProofError):
                self.pending.parse(value)
        self.assertEqual(self.pending.parse(self.url(code="code#" + valid)).reveal(), "code")
        self.assertEqual(self.pending.parse("provider-issued-bare-code").reveal(), "provider-issued-bare-code")

    def test_pending_expiry(self):
        with patch.object(self.pending, "_clock", return_value=self.pending.deadline):
            with self.assertRaisesRegex(compat.ProofError, "login_timeout"):
                self.pending.parse("code")

    def test_listener_fallback_and_loopback_only(self):
        with socket.socket() as occupied:
            try:
                occupied.bind(("127.0.0.1", 54545))
            except OSError:
                pass  # Already occupied still exercises fallback.
            callbacks = proof.Callbacks()
            try:
                self.assertTrue(callbacks.fallback)
                for listener in callbacks.sockets:
                    self.assertIn(listener.getsockname()[0], ("127.0.0.1", "::1"))
                pending = proof.PendingLogin(callbacks.redirect_uri)
                for listener in callbacks.sockets:
                    host, port, *_ = listener.getsockname()
                    with socket.create_connection((host, port)) as client:
                        target = "/callback?" + urllib.parse.urlencode({"code": "test", "state": pending.state.reveal()})
                        client.sendall(f"GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n".encode())
                        code = callbacks.accept(listener, pending)
                        response = client.recv(4096)
                        self.assertEqual(code.reveal(), "test")
                        self.assertNotIn(b"test", response)
                        self.assertNotIn(pending.state.reveal().encode(), response)
            finally:
                callbacks.close()
            self.assertTrue(all(s.fileno() == -1 for s in callbacks.sockets))

    def test_headless_hidden_input_transport_and_cancellation(self):
        callbacks = proof.Callbacks(random_port=True)
        read_fd, write_fd = os.pipe()
        try:
            pending = proof.PendingLogin(callbacks.redirect_uri)
            os.write(write_fd, b"wrong#wrong\n")
            notices = []

            def notice(message):
                notices.append(message)
                os.write(write_fd, b"correct-provider-code\n")

            code, mode = proof.wait_for_callback(callbacks, pending, read_fd, notice)
            self.assertEqual(mode, "manual")
            self.assertEqual(code.reveal(), "correct-provider-code")
            self.assertNotIn("wrong#wrong", "".join(notices))
            with self.assertRaisesRegex(compat.ProofError, "login_cancelled"):
                proof.wait_for_callback(callbacks, pending, read_fd, notice, cancelled=lambda: True)
        finally:
            callbacks.close()
            os.close(read_fd)
            os.close(write_fd)

    def test_callback_trickle_cannot_extend_connection_budget(self):
        class Trickle:
            def __init__(self):
                self.receives = 0

            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

            def settimeout(self, seconds):
                pass

            def recv(self, size):
                self.receives += 1
                return b"G"

            def sendall(self, value):
                self.response = value

        connection = Trickle()
        listener = unittest.mock.Mock()
        listener.accept.return_value = (connection, ("127.0.0.1", 12345))
        callbacks = proof.Callbacks(random_port=True)
        try:
            with patch.object(proof.time, "monotonic", side_effect=[0, 0.4, 0.8, 1.2]):
                self.assertIsNone(callbacks.accept(listener, self.pending))
            self.assertEqual(connection.receives, 2)
            self.assertIn(b"400 Bad Request", connection.response)
        finally:
            callbacks.close()

    def test_browser_failure_preserves_manual_flow_and_terminal_echo(self):
        callbacks = proof.Callbacks(random_port=True)
        master, slave = pty.openpty()
        original = proof.termios.tcgetattr(slave)
        pending = proof.PendingLogin(callbacks.redirect_uri)
        duplicate = os.dup(slave)

        def wait(callbacks, pending, fd, notice):
            self.assertFalse(proof.termios.tcgetattr(fd)[3] & proof.termios.ECHO)
            return pending.parse("SYNTHETIC_PRIVATE_CODE"), "manual"

        try:
            with patch.object(proof.os, "open", return_value=duplicate), patch.object(proof.webbrowser, "open", return_value=False), patch.object(proof, "wait_for_callback", side_effect=wait):
                with contextlib.redirect_stdout(io.StringIO()) as stdout, contextlib.redirect_stderr(io.StringIO()) as stderr:
                    code, mode = proof.browser_login(callbacks, pending, False)
            self.assertEqual(mode, "manual")
            self.assertEqual(code.reveal(), "SYNTHETIC_PRIVATE_CODE")
            self.assertEqual(proof.termios.tcgetattr(slave), original)
            self.assertEqual(stdout.getvalue() + stderr.getvalue(), "")
            private_output = os.read(master, 8192)
            self.assertIn(b"Browser launch failed", private_output)
            self.assertNotIn(b"SYNTHETIC_PRIVATE_CODE", private_output)
        finally:
            callbacks.close()
            os.close(master)
            os.close(slave)

    def private_tty_login(self, mode):
        """Use a new controlling terminal, not a mocked /dev/tty or slave FD.

        All browser/HTTPS work is replaced in the child. The parent drains and
        discards private terminal output; only fixed checks/error labels return.
        """
        private_code = b"SYNTHETIC_PRIVATE_TTY_CODE"
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp).resolve()
            read_fd, write_fd = os.pipe()
            pid, master = pty.fork()
            if pid == 0:
                os.close(read_fd)
                summary = {}
                try:
                    tty = os.open("/dev/tty", os.O_RDWR | os.O_NOCTTY)
                    before = proof.termios.tcgetattr(tty)
                    os.close(tty)
                    workers, browser_checks, listeners, requests = [], [], [], []
                    original_callbacks = proof.Callbacks

                    def callbacks(random_port):
                        instance = original_callbacks(random_port)
                        listeners.append(instance)
                        return instance

                    def browser(url, new):
                        if mode in ("manual", "cancel"):
                            return False
                        query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
                        redirect = urllib.parse.urlsplit(query["redirect_uri"][0])
                        self.assertEqual(redirect.hostname, "localhost")
                        target = "/callback?" + urllib.parse.urlencode({"code": private_code.decode(), "state": query["state"][0]})

                        def deliver():
                            try:
                                with socket.create_connection(("127.0.0.1", redirect.port), timeout=2) as client:
                                    client.sendall(f"GET {target} HTTP/1.1\r\nHost: localhost:{redirect.port}\r\n\r\n".encode())
                                    browser_checks.append(client.recv(4096).startswith(b"HTTP/1.1 200 OK"))
                            except Exception:
                                browser_checks.append(False)

                        worker = threading.Thread(target=deliver, daemon=True)
                        workers.append(worker)
                        worker.start()
                        return True

                    def token_response(request, timeout):
                        # This is the real Transport/JSON reader with a synthetic
                        # opener. No HTTP connection or provider login is made.
                        self.assertEqual(request.full_url, compat.API_ORIGIN + "/v1/oauth/token")
                        body = json.loads(request.data)
                        self.assertEqual(body["code"], private_code.decode())
                        self.assertEqual(body["grant_type"], "authorization_code")
                        requests.append(True)
                        if mode == "failure":
                            raise ValueError(ACCESS + REFRESH + private_code.decode())
                        return io.BytesIO(compat.json_bytes(TOKEN))

                    argv = [str(Path(proof.__file__)), "--state-dir", str(directory), "login", "--random-port"]
                    with patch.object(sys, "argv", argv), patch.object(proof, "Callbacks", side_effect=callbacks), patch.object(proof.webbrowser, "open", side_effect=browser), patch.object(urllib.request.OpenerDirector, "open", side_effect=token_response):
                        with contextlib.redirect_stdout(io.StringIO()) as stdout, contextlib.redirect_stderr(io.StringIO()) as stderr:
                            returncode = proof.run_cli()
                    tty = os.open("/dev/tty", os.O_RDWR | os.O_NOCTTY)
                    restored = proof.termios.tcgetattr(tty) == before
                    os.close(tty)
                    for worker in workers:
                        worker.join(3)
                    public = stdout.getvalue() + stderr.getvalue()
                    summary = {
                        "returncode": returncode, "terminal_restored": restored,
                        "listeners_closed": all(s.fileno() == -1 for c in listeners for s in c.sockets),
                        "token_exchanges": len(requests), "grant_saved": (directory / "grant.json").exists(),
                        "public_redacted": all(v not in public for v in (ACCESS, REFRESH, private_code.decode(), ACCOUNT, ORGANIZATION, "https://", "http://", "Traceback")),
                        "callback_completed": browser_checks == [True],
                    }
                    if stderr.getvalue():
                        summary["error"] = json.loads(stderr.getvalue())["error"]
                    else:
                        summary["callback_mode"] = json.loads(stdout.getvalue())["result"]["callback_mode"]
                except Exception as error:
                    summary = {"harness_failure": proof.error_class(error)}
                os.write(write_fd, json.dumps(summary).encode())
                os.close(write_fd)
                os._exit(0)

            os.close(write_fd)
            result, private_output = bytearray(), bytearray()
            sent = False
            reaped = False
            try:
                with proof.selectors.SelectSelector() as selector:
                    selector.register(master, proof.selectors.EVENT_READ, "terminal")
                    selector.register(read_fd, proof.selectors.EVENT_READ, "result")
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        for key, _ in selector.select(0.1):
                            try:
                                chunk = os.read(key.fd, 8192)
                            except OSError:
                                chunk = b""
                            if not chunk:
                                selector.unregister(key.fd)
                                continue
                            if key.data == "result":
                                result.extend(chunk)
                            else:
                                private_output.extend(chunk)
                                if mode in ("manual", "cancel") and not sent and b"Waiting up to five minutes" in private_output:
                                    os.write(master, b"\x03" if mode == "cancel" else private_code + b"\n")
                                    sent = True
                        if os.waitpid(pid, os.WNOHANG)[0]:
                            reaped = True
                            if not result:
                                result.extend(os.read(read_fd, 8192))
                            break
                self.assertTrue(reaped, "Synthetic private-terminal login did not finish")
                self.assertTrue(private_code not in private_output, "Hidden manual callback input was echoed")
                self.assertTrue(ACCESS.encode() not in private_output and REFRESH.encode() not in private_output, "Private-terminal output contained token canaries")
                return json.loads(result)
            finally:
                if not reaped:
                    os.kill(pid, proof.signal.SIGKILL)
                    os.waitpid(pid, 0)
                os.close(master)
                os.close(read_fd)

    def test_controlling_dev_tty_loopback_login(self):
        result = self.private_tty_login("loopback")
        self.assertEqual(result.get("returncode"), 0)
        self.assertEqual(result["callback_mode"], "loopback")
        self.assertEqual(result["token_exchanges"], 1)
        self.assertTrue(result["callback_completed"] and result["grant_saved"])
        self.assertTrue(result["terminal_restored"] and result["listeners_closed"] and result["public_redacted"])

    def test_controlling_dev_tty_manual_login(self):
        result = self.private_tty_login("manual")
        self.assertEqual(result.get("returncode"), 0)
        self.assertEqual(result["callback_mode"], "manual")
        self.assertEqual(result["token_exchanges"], 1)
        self.assertTrue(result["grant_saved"] and result["terminal_restored"] and result["listeners_closed"] and result["public_redacted"])

    def test_controlling_dev_tty_cancellation_restores_terminal(self):
        result = self.private_tty_login("cancel")
        self.assertEqual(result.get("returncode"), 130)
        self.assertEqual(result["error"]["stage"], "callback_wait")
        self.assertEqual(result["error"]["error_class"], "cancelled")
        self.assertEqual(result["token_exchanges"], 0)
        self.assertTrue(not result["grant_saved"] and result["terminal_restored"] and result["listeners_closed"] and result["public_redacted"])

    def test_controlling_dev_tty_exchange_exception_is_payload_free(self):
        result = self.private_tty_login("failure")
        self.assertEqual(result.get("returncode"), 1)
        self.assertEqual(result["error"]["kind"], "diagnostic_failed")
        self.assertEqual(result["error"]["stage"], "token_exchange")
        self.assertEqual(result["error"]["error_class"], "value_error")
        self.assertEqual(result["token_exchanges"], 1)
        self.assertTrue(not result["grant_saved"] and result["terminal_restored"] and result["listeners_closed"] and result["public_redacted"])


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name).resolve() / "private" / "nested"
        self.store = proof.Store(self.path)

    def tearDown(self):
        self.store.close()
        self.tmp.cleanup()

    def test_private_atomic_persistence_and_restart(self):
        value = record()
        with self.store.locked():
            self.store.write(value)
        self.assertEqual(self.store.load(), value)
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.path.parent.stat().st_mode & 0o777, 0o700)
        for name in ("grant.json", "grant.lock"):
            self.assertEqual((self.path / name).stat().st_mode & 0o777, 0o600)
        other = proof.Store(self.path)
        try:
            self.assertEqual(other.load(), value)
            output = proof.status(other)
            self.assertNotIn(ACCESS, json.dumps(output))
            self.assertNotIn(REFRESH, json.dumps(output))
        finally:
            other.close()

    def test_stable_lock_serializes_independent_process_across_replace(self):
        old = record()
        self.store.write(old)
        script = """
import sys
sys.path.insert(0, sys.argv[1])
import proof
store = proof.Store(sys.argv[2])
try:
    print('started', flush=True)
    with store.locked():
        print(store.load()['revision'], flush=True)
finally:
    store.close()
"""
        child = None
        try:
            with self.store.locked():
                child = subprocess.Popen([sys.executable, "-u", "-c", script, str(Path(proof.__file__).parent), str(self.path)],
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                with proof.selectors.DefaultSelector() as selector:
                    selector.register(child.stdout, proof.selectors.EVENT_READ)
                    self.assertTrue(selector.select(5), "child did not start")
                    self.assertEqual(child.stdout.readline(), b"started\n")
                    self.store.write({**old, "revision": 2})
                    self.assertFalse(selector.select(0.1), "replacing grant must not replace/bypass the held lock")
            stdout, stderr = child.communicate(timeout=5)
            self.assertEqual(child.returncode, 0, stderr.decode())
            self.assertEqual(stdout, b"2\n")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.communicate()

    def test_unsafe_path_and_file_refused_without_chmod(self):
        (self.path / "grant.json").symlink_to(self.path / "other")
        with self.assertRaises((compat.ProofError, OSError)):
            self.store.load()
        with self.assertRaises((compat.ProofError, OSError)):
            self.store.write(record())
        (self.path / "grant.json").unlink()
        unsafe = self.path.parent / "public"
        unsafe.mkdir(mode=0o755)
        with self.assertRaisesRegex(compat.ProofError, "credential_storage_error"):
            proof.Store(unsafe)
        self.assertEqual(unsafe.stat().st_mode & 0o777, 0o755)
        link = self.path.parent / "linked"
        link.symlink_to(self.path, target_is_directory=True)
        with self.assertRaises(compat.ProofError):
            proof.Store(link)

    def test_unknown_schema_and_failed_atomic_replace_preserve_old(self):
        old = record()
        self.store.write(old)
        with patch.object(proof.os, "replace", side_effect=OSError("SYNTHETIC_SECRET")):
            with self.assertRaisesRegex(compat.ProofError, "credential_storage_error"):
                self.store.write(record())
        self.assertEqual(self.store.load(), old)
        self.assertEqual(list(self.path.glob(".grant-*")), [])
        self.store.write({**old, "schema": "future"})
        with self.assertRaisesRegex(compat.ProofError, "unknown_diagnostic_store_schema"):
            self.store.load()

    def test_unsafe_ancestor_and_swapped_directory_are_rejected(self):
        unsafe = self.path.parent / "writable-parent"
        unsafe.mkdir(mode=0o700)
        unsafe.chmod(0o777)
        try:
            with self.assertRaisesRegex(compat.ProofError, "credential_storage_error"):
                proof.Store(unsafe / "new-private")
            self.assertFalse((unsafe / "new-private").exists())
        finally:
            unsafe.chmod(0o700)

        target = self.path.parent / "target"
        target.mkdir(mode=0o700)
        actual_open = proof.os.open

        def race(path, flags, *args, **kwargs):
            if path == "target":
                target.chmod(0o755)
            return actual_open(path, flags, *args, **kwargs)

        with patch.object(proof.os, "open", side_effect=race):
            with self.assertRaisesRegex(compat.ProofError, "credential_storage_error"):
                proof.Store(target)

    def test_oversized_record_cannot_replace_reloadable_grant(self):
        old = record()
        self.store.write(old)
        oversized = {**old, "extra": "x" * proof.MAX_STORE}
        with self.assertRaisesRegex(compat.ProofError, "credential_storage_error"):
            self.store.write(oversized)
        self.assertEqual(self.store.load(), old)

    def test_directory_flush_failure_reports_commit_uncertainty_without_rollback(self):
        old = record()
        self.store.write(old)
        replacement = {**old, "revision": 2}
        actual_fsync = proof.os.fsync

        def fail_directory(fd):
            if fd == self.store.dir_fd:
                raise OSError("SYNTHETIC_STORAGE_CANARY")
            return actual_fsync(fd)

        with patch.object(proof.os, "fsync", side_effect=fail_directory):
            with self.assertRaisesRegex(compat.ProofError, "credential_commit_outcome_unknown") as caught:
                self.store.write(replacement)
        self.assertEqual(self.store.load(), replacement)
        self.assertNotIn("SYNTHETIC_STORAGE_CANARY", json.dumps(proof.safe_error(caught.exception)))

    def test_real_refresh_request_profile_and_logout_tombstone(self):
        old = record()
        self.store.write(old)
        transport = FakeTransport([{**TOKEN, "access_token": ACCESS + "2", "refresh_token": REFRESH + "2"}])
        result = proof.refresh(self.store, transport)
        current = self.store.load()
        self.assertTrue(result["real_token_endpoint_refresh"])
        self.assertEqual(current["generation"], old["generation"])
        self.assertEqual(current["revision"], 2)
        self.assertEqual(transport.calls[0][2], {"grant_type": "refresh_token", "client_id": compat.CLIENT_ID, "refresh_token": REFRESH})
        proof.logout(self.store)
        logged_out = self.store.load()
        self.assertNotEqual(logged_out["generation"], old["generation"])
        self.assertIsNone(logged_out["credential"])
        with self.assertRaisesRegex(compat.ProofError, "login_required"):
            proof.refresh(self.store, FakeTransport([]))

    def test_refresh_failure_retains_old_credentials(self):
        old = record()
        self.store.write(old)
        for response in ({**TOKEN, "refresh_token": ""}, compat.ProofError("transport_outcome_unknown")):
            with self.assertRaises(compat.ProofError):
                proof.refresh(self.store, FakeTransport([response]))
            self.assertEqual(self.store.load(), old)

    def test_login_bootstrap_and_stale_login_guard(self):
        token_without_identity = {k: v for k, v in TOKEN.items() if k not in ("account", "organization")}
        transport = FakeTransport([token_without_identity, {"oauth_account": {"account_uuid": ACCOUNT, "organization_uuid": ORGANIZATION}}])

        def browser(callbacks, pending, no_browser):
            return pending.parse("synthetic-fresh-code"), "manual"

        with patch.object(proof, "browser_login", browser):
            result = proof.login(self.store, transport, True, True)
        self.assertTrue(result["fresh_mirror_owned_grant"])
        self.assertEqual([c[0] for c in transport.calls], ["exchange", "bootstrap"])
        self.assertEqual(self.store.grant(self.store.load()).account_id, ACCOUNT)
        old = self.store.load()

        def stale_browser(callbacks, pending, no_browser):
            proof.logout(self.store)
            return pending.parse("synthetic-stale-code"), "manual"

        with patch.object(proof, "browser_login", stale_browser):
            with self.assertRaisesRegex(compat.ProofError, "newer_login_or_logout_won"):
                proof.login(self.store, FakeTransport([TOKEN]), True, True)
        self.assertNotEqual(self.store.load()["generation"], old["generation"])
        self.assertIsNone(self.store.load()["credential"])

    def test_login_failure_keeps_old_and_never_bootstraps_api_key(self):
        old = record()
        self.store.write(old)
        for token in ({**TOKEN, "access_token": "sk-ant-api01-SYNTHETIC"}, {**TOKEN, "account": {"uuid": "invalid"}}):
            transport = FakeTransport([token])
            with patch.object(proof, "browser_login", lambda c, p, n: (p.parse("synthetic-code"), "manual")):
                with self.assertRaises(compat.ProofError):
                    proof.login(self.store, transport, True, True)
            self.assertEqual(len(transport.calls), 1)
            self.assertEqual(self.store.load(), old)


class WireTests(unittest.TestCase):
    def test_golden_request_and_bun_hash_vectors(self):
        fixture = json.loads((Path(__file__).parent / "fixtures" / "wire.json").read_text())
        actual = compat.request_body(fixture["model"], fixture["canonical_messages"], INSTALLATION, ACCOUNT, SESSION,
                                     tools=fixture["tools"], tool_choice=fixture["tool_choice"])
        self.assertEqual(actual.decode(), fixture["request_utf8"])
        for vector in fixture["xxhash64_bun_vectors"]:
            self.assertEqual(f"{compat.xxhash64(bytes.fromhex(vector['input_hex']), vector['seed']):016x}", vector["expected"])
        original = fixture["canonical_messages"][0]["content"]
        self.assertEqual(json.loads(actual)["messages"][0]["content"], original)
        self.assertIn("cch=00000", original)

    def test_explicit_reversible_tool_mapping(self):
        names = compat.ToolNames([{"name": "read"}, {"name": "_read"}, {"name": "extension_read"}])
        for name in names.forward:
            self.assertEqual(names.decode(names.encode(name)), name)
        for tools in ([{"name": "read"}, {"name": "read"}], [{"name": "x" * 64}], [{"name": "ext.bad"}]):
            with self.assertRaises(compat.ProofError):
                compat.ToolNames(tools)
        with self.assertRaisesRegex(compat.ProofError, "unknown_tool_name"):
            names.decode("_unregistered")

    def test_profiles_capabilities_and_no_api_key(self):
        for model in compat.MODELS:
            value = json.loads(compat.request_body(model, [{"role": "user", "content": "hello"}], INSTALLATION, ACCOUNT, SESSION))
            self.assertEqual(value["thinking"], compat.MODELS[model])
            self.assertEqual(value["output_config"], {"effort": "low"})
        for extra in ({"metadata": {}}, {"system": []}, {"speed": "fast"}, {"max_tokens": 1}):
            with self.assertRaisesRegex(compat.ProofError, "conflicting_extra_body"):
                compat.request_body("claude-opus-4-8", [], INSTALLATION, ACCOUNT, SESSION, extra_body=extra)
        for operation in ("exchange", "refresh", "bootstrap", "models", "messages"):
            result = compat.headers(operation, proof.Secret(ACCESS), SESSION)
            self.assertNotIn("x-api-key", {k.lower() for k in result})
        self.assertNotIn("anthropic-beta", compat.headers("exchange"))
        self.assertEqual(compat.headers("refresh")["anthropic-beta"], compat.OAUTH_BETA)

    def test_forced_tool_matches_pinned_omp_thinking_rule(self):
        for model in compat.MODELS:
            body = json.loads(compat.request_body(model, [{"role": "user", "content": "Call the echo tool."}],
                              INSTALLATION, ACCOUNT, SESSION,
                              tool_choice={"type": "tool", "name": compat.ECHO_NAME}))
            self.assertEqual(body["tool_choice"], {"type": "tool", "name": "_" + compat.ECHO_NAME})
            self.assertNotIn("thinking", body)
            self.assertNotIn("context_management", body)
            self.assertEqual(body["output_config"], {"effort": "low"})

    def test_signed_reasoning_stream_and_exact_replay(self):
        events = [{"type": "message_start"},
                  {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}},
                  {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "synthetic"}},
                  {"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "signed-canary"}},
                  {"type": "content_block_stop", "index": 0},
                  {"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "tool_canary", "name": "_" + compat.ECHO_NAME, "input": {}}},
                  {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": '{"value":"Mirror π 🪞 phase0"}'}},
                  {"type": "content_block_stop", "index": 1},
                  {"type": "message_delta", "delta": {"stop_reason": "tool_use"}}, {"type": "message_stop"}]
        blocks, reason = proof.parse_stream(io.BytesIO(sse(events)), compat.ToolNames(compat.TOOLS))
        self.assertEqual(reason, "tool_use")
        value = json.loads(compat.request_body("claude-opus-4-8", [{"role": "user", "content": "first"}, {"role": "assistant", "content": blocks}], INSTALLATION, ACCOUNT, SESSION))
        self.assertEqual(value["messages"][1]["content"][0], blocks[0])
        self.assertEqual(value["messages"][1]["content"][1]["id"], "tool_canary")
        self.assertEqual(blocks[1]["name"], compat.ECHO_NAME)
        for truncated in (events[:-1], events[:5], events[:8]):
            with self.assertRaisesRegex(compat.ProofError, "partial_stream_error"):
                proof.parse_stream(io.BytesIO(sse(truncated)), compat.ToolNames(compat.TOOLS))

    def test_unknown_returned_tool_and_unsigned_reasoning_fail(self):
        cases = [({"type": "tool_use", "id": "tool", "name": "_not_registered", "input": {}}, "unknown_tool_name"),
                 ({"type": "thinking", "thinking": "synthetic", "signature": ""}, "unsigned_thinking")]
        for block, error in cases:
            with self.assertRaisesRegex(compat.ProofError, error):
                proof.parse_stream(io.BytesIO(sse(response_events([block]))), compat.ToolNames(compat.TOOLS))

    def test_long_opaque_reasoning_is_preserved_within_stream_limit(self):
        originals = [{"type": "thinking", "thinking": "synthetic", "signature": "s" * 16384},
                     {"type": "redacted_thinking", "data": "r" * 16384}]
        blocks, reason = proof.parse_stream(io.BytesIO(sse(response_events(originals))), compat.ToolNames(compat.TOOLS))
        body = json.loads(compat.request_body("claude-opus-4-8", [{"role": "user", "content": "first"},
                              {"role": "assistant", "content": blocks}], INSTALLATION, ACCOUNT, SESSION))
        self.assertEqual(body["messages"][1]["content"], originals)

    def test_stream_trickle_obeys_budget_without_waiting_for_newline(self):
        class Trickle:
            def __init__(self):
                self.reads = 0

            def read1(self, size):
                self.reads += 1
                return b"d"

        response = Trickle()
        transport = unittest.mock.Mock()
        transport.response.return_value = contextlib.nullcontext(response)
        with patch.object(proof.time, "monotonic", side_effect=[0, 20, 40, 60]):
            with self.assertRaisesRegex(compat.ProofError, "partial_stream_error"):
                proof.complete(transport, grant(), record(), "claude-opus-4-8", SESSION, [{"role": "user", "content": "test"}])
        self.assertEqual(response.reads, 2)

    def test_model_detail_must_match_requested_model_before_inference(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = proof.Store(Path(tmp).resolve())
            try:
                store.write(record())
                transport = FakeTransport([{"data": [{"id": "claude-opus-4-8"}], "has_more": False},
                                           {"id": "other-model"}])
                with self.assertRaisesRegex(compat.ProofError, "invalid_model_response"):
                    proof.probe(store, transport, "claude-opus-4-8")
                self.assertEqual(len(transport.calls), 2)
            finally:
                store.close()

    def test_model_pagination_and_unknown_limits(self):
        transport = FakeTransport([{"data": [{"id": "first"}], "has_more": True, "last_id": "first"},
                                   {"data": [{"id": "second", "max_input_tokens": 12345}], "has_more": False}])
        self.assertEqual(proof.models(transport, grant()), [{"id": "first"}, {"id": "second", "max_input_tokens": 12345}])
        self.assertEqual(transport.calls[1][3]["after"], "first")
        self.assertEqual(proof.model_summary({"id": "claude-opus-4-8"}), {"id": "claude-opus-4-8"})
        with self.assertRaises(compat.ProofError):
            proof.models(FakeTransport([{"data": [], "has_more": True, "last_id": "missing"}]), grant())

    def test_synthetic_three_turn_tool_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = proof.Store(Path(tmp).resolve())
            try:
                store.write(record())
                transport = FakeTransport([
                    {"data": [{"id": "claude-opus-4-8"}], "has_more": False}, {"id": "claude-opus-4-8"},
                    sse(response_events([{"type": "text", "text": "MIRROR_PHASE0_TEXT_OK"}])),
                    sse(response_events([{"type": "thinking", "thinking": "synthetic", "signature": "signature-unchanged"},
                                         {"type": "tool_use", "id": "call-one", "name": "_" + compat.ECHO_NAME, "input": {"value": compat.ECHO_VALUE}}], "tool_use")),
                    sse(response_events([{"type": "text", "text": "MIRROR_PHASE0_TOOL_OK"}])),
                ])
                result = proof.probe(store, transport, "claude-opus-4-8")
                self.assertEqual(result["signed_reasoning_blocks_replayed"], 1)
                final = json.loads(transport.calls[-1][2])
                self.assertEqual(final["messages"][-1]["content"][0]["tool_use_id"], "call-one")
                self.assertEqual(final["messages"][-2]["content"][0]["signature"], "signature-unchanged")
                self.assertNotIn(ACCESS, json.dumps(result))
            finally:
                store.close()


class SafetyTests(unittest.TestCase):
    def test_diagnostic_stages_and_classes_never_include_exception_payloads(self):
        custom = type("SYNTHETIC_SECRET_CLASS_NAME", (Exception,), {})
        for error, classification in ((OSError(22, ACCESS), "os_error"),
                                      (ValueError(REFRESH), "value_error"),
                                      (custom(ACCOUNT, ACCESS, REFRESH), "unexpected_error")):
            with self.assertRaises(compat.ProofError) as caught:
                with proof.diagnostic_stage("callback_wait"):
                    raise error
            output = proof.safe_error(caught.exception)
            self.assertEqual(output["error"]["stage"], "callback_wait")
            self.assertEqual(output["error"]["error_class"], classification)
            encoded = json.dumps(output)
            self.assertTrue(all(value not in encoded for value in (ACCESS, REFRESH, ACCOUNT, custom.__name__)))

    def test_diagnostic_labels_are_allowlisted_and_inner_stage_wins(self):
        with self.assertRaises(compat.ProofError) as caught:
            with proof.diagnostic_stage("command"):
                with proof.diagnostic_stage("token_validation"):
                    raise compat.ProofError("invalid_token_response")
        self.assertEqual(proof.safe_error(caught.exception)["error"]["stage"], "token_validation")
        for label in (ACCESS, [REFRESH]):
            output = proof.safe_error(proof.DiagnosticError("diagnostic_failed", stage=label, classification=label))
            self.assertEqual(output["error"]["stage"], "command")
            self.assertEqual(output["error"]["error_class"], "unexpected_error")
            self.assertTrue(ACCESS not in json.dumps(output) and REFRESH not in json.dumps(output))

    def test_http_header_deadline_interrupts_and_restores_signal_handler(self):
        transport = proof.Transport()
        previous = proof.signal.getsignal(proof.signal.SIGALRM)
        def stalled_headers(*args, **kwargs):
            time.sleep(5)
            self.fail("HTTP header budget failed to interrupt")
        with patch.object(transport.opener, "open", side_effect=stalled_headers) as send:
            with self.assertRaisesRegex(compat.ProofError, "transport_outcome_unknown"):
                transport.json("models", proof.Secret(ACCESS), timeout=0.03)
        self.assertEqual(send.call_count, 1)
        self.assertEqual(proof.signal.getsignal(proof.signal.SIGALRM), previous)
        self.assertEqual(proof.signal.getitimer(proof.signal.ITIMER_REAL), (0, 0))

    def test_environment_endpoint_and_proxy_isolation(self):
        with patch.dict(os.environ, {"ANTHROPIC_BASE_URL": "https://evil.test"}):
            with self.assertRaisesRegex(compat.ProofError, "invalid_configuration"):
                proof.Transport()
        with patch.dict(os.environ, {"HTTPS_PROXY": "https://evil.test", "ANTHROPIC_API_KEY": "API_KEY_CANARY", "CLAUDE_CODE_OAUTH_TOKEN": "TOKEN_ENV_CANARY"}, clear=True):
            transport = proof.Transport()
            proxies = [h for h in transport.opener.handlers if isinstance(h, urllib.request.ProxyHandler)]
            self.assertTrue(all(h.proxies == {} for h in proxies))
            self.assertIsNone(proof.NoRedirect().redirect_request(None, None, 302, "", {}, "https://evil.test"))
            with self.assertRaisesRegex(compat.ProofError, "invalid_configuration"):
                with transport.response("https://evil.test", proof.Secret(ACCESS)):
                    self.fail("unexpected dispatch")

    def test_redirect_and_provider_errors_never_echo_raw_payloads_or_retry(self):
        for status in (302, 400, 401, 403, 429, 503):
            with self.subTest(status=status):
                transport = proof.Transport()
                error = proof.urllib.error.HTTPError("https://api.anthropic.com/SYNTHETIC_SECRET", status,
                                                    "SYNTHETIC_SECRET", {"Location": "https://evil.test"}, io.BytesIO(b"SYNTHETIC_SECRET"))
                with patch.object(transport.opener, "open", side_effect=error) as send:
                    with self.assertRaises(compat.ProofError) as caught:
                        transport.json("refresh", data={"refresh_token": REFRESH})
                    self.assertNotIn("SYNTHETIC_SECRET", str(caught.exception))
                    self.assertEqual(caught.exception.status, status)
                    self.assertEqual(send.call_count, 1)

    def test_safe_cli_errors_do_not_echo_unknown_arguments(self):
        script = Path(proof.__file__)
        result = subprocess.run([sys.executable, str(script), "--secret-code=SYNTHETIC_CODE_CANARY"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("SYNTHETIC_CODE_CANARY", result.stdout + result.stderr)
        self.assertIn("invalid_command_line", result.stderr)
        self.assertIn("recovery", json.loads(result.stderr)["error"])


if __name__ == "__main__":
    unittest.main()
