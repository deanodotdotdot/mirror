#!/usr/bin/env python3
"""Isolated, explicit Phase 0 OAuth contract diagnostic. See README.md."""

import argparse
import base64
import contextlib
import datetime
import errno
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import secrets
import selectors
import signal
import socket
import stat
import subprocess
import sys
import termios
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import webbrowser

from compatibility import (
    API_ORIGIN, AUTHORIZATION_URL, CLIENT_ID, ECHO_NAME, ECHO_VALUE, MODELS,
    PROFILE, SCOPES, SOURCE_REVISION, TOOLS, ProofError, ToolNames,
    headers, json_bytes, request_body,
)

MAX_BODY = 2 * 1024 * 1024
MAX_CALLBACK = 8192
MAX_STORE = 16384
STORE_SCHEMA = "mirror-claude-phase0-private-v1"
SAFE_STAGES = frozenset({
    "command", "credential_load", "callback_listener", "terminal_open",
    "terminal_setup", "callback_wait", "terminal_cleanup", "token_exchange",
    "token_validation", "identity_bootstrap", "identity_validation",
    "credential_commit", "callback_cleanup",
})
SAFE_ERROR_CLASSES = frozenset({
    "controlled_error", "cancelled", "os_error", "value_error", "type_error",
    "attribute_error", "key_error", "runtime_error", "unexpected_error",
})


def error_class(error):
    # Classify into fixed public constants. Never expose an exception's name,
    # message, arguments, attributes, cause, or traceback: any can carry secrets.
    for cls, label in ((OSError, "os_error"), (ValueError, "value_error"),
                       (TypeError, "type_error"), (AttributeError, "attribute_error"),
                       (KeyError, "key_error"), (RuntimeError, "runtime_error")):
        if isinstance(error, cls):
            return label
    return "unexpected_error"


class DiagnosticError(ProofError):
    def __init__(self, kind, *, stage, classification):
        super().__init__(kind)
        self.stage = stage
        self.classification = classification


@contextlib.contextmanager
def diagnostic_stage(stage):
    try:
        yield
    except ProofError as error:
        if not getattr(error, "stage", None):
            error.stage = stage
        raise
    except KeyboardInterrupt:
        raise DiagnosticError("login_cancelled", stage=stage, classification="cancelled") from None
    except Exception as error:
        raise DiagnosticError("diagnostic_failed", stage=stage, classification=error_class(error)) from None


class Secret:
    def __init__(self, value):
        self._value = value

    def __repr__(self):
        return "Secret(<redacted>)"

    __str__ = __repr__

    def reveal(self):
        return self._value


def utc(timestamp=None):
    return datetime.datetime.fromtimestamp(time.time() if timestamp is None else timestamp,
                                          datetime.timezone.utc).isoformat()


def nonempty(value):
    return isinstance(value, str) and 0 < len(value) <= 8192 and not any(ord(c) < 32 for c in value)


def identity(value):
    try:
        if not isinstance(value, str) or str(uuid.UUID(value)) != value.lower():
            raise ValueError()
    except (ValueError, AttributeError):
        raise ProofError("invalid_identity") from None
    return value.lower()


class Grant:
    """Secrets have no debug/JSON representation outside the private store."""

    def __init__(self, access, refresh, expires_at, account_id, organization_id, scopes,
                 authorized_at=None):
        self.access = Secret(access)
        self.refresh = Secret(refresh)
        self.expires_at = expires_at
        self.account_id = identity(account_id)
        self.organization_id = identity(organization_id)
        self.scopes = scopes
        self.authorized_at = authorized_at if authorized_at is not None else time.time()

    @classmethod
    def from_token(cls, data, previous=None, now=None):
        now = time.time() if now is None else now
        if not isinstance(data, dict):
            raise ProofError("invalid_token_response")
        access = data.get("access_token")
        refresh = data.get("refresh_token", previous.refresh.reveal() if previous else None)
        expiry = data.get("expires_in")
        token_type = data.get("token_type")
        scopes = data.get("scope", previous.scopes if previous else None)
        if isinstance(scopes, str):
            scopes = scopes.split()
        if (not nonempty(access) or not access.startswith("sk-ant-oat") or not nonempty(refresh)
                or not isinstance(token_type, str) or token_type.lower() != "bearer"
                or not isinstance(expiry, int) or isinstance(expiry, bool) or not 0 < expiry <= 366 * 86400
                or not isinstance(scopes, list) or not all(nonempty(s) for s in scopes)
                or "user:inference" not in scopes):
            raise ProofError("invalid_token_response")
        account = data.get("account", {})
        organization = data.get("organization", {})
        if not isinstance(account, dict) or not isinstance(organization, dict):
            raise ProofError("invalid_token_response")
        account_id = account.get("uuid", previous.account_id if previous else None)
        organization_id = organization.get("uuid", previous.organization_id if previous else None)
        if previous and (account_id != previous.account_id or organization_id != previous.organization_id):
            raise ProofError("account_changed")
        return cls(access, refresh, now + expiry, account_id, organization_id, scopes,
                   previous.authorized_at if previous else now)

    def private_record(self):
        return {"access_token": self.access.reveal(), "refresh_token": self.refresh.reveal(),
                "expires_at": self.expires_at, "account_id": self.account_id,
                "organization_id": self.organization_id, "scopes": self.scopes,
                "authorized_at": self.authorized_at}


class Store:
    """Private spike storage; deliberately incompatible with the production spec.

    The stable lock covers an entire diagnostic operation, including its bounded
    network work. Phase 1 must implement production dispatch/renewal semantics.
    """

    def __init__(self, directory):
        self.directory = Path(directory).expanduser().absolute()
        if ".." in self.directory.parts:
            raise ProofError("credential_storage_error")
        fd = None
        try:
            # Walk from an open root, validating the actual descriptors rather
            # than lstat then reopening a swappable pathname. Never follow links.
            fd = os.open(self.directory.anchor, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            for component in self.directory.parts[1:]:
                self._check_directory(fd)
                try:
                    os.mkdir(component, mode=0o700, dir_fd=fd)
                except FileExistsError:
                    pass
                child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = child
            self._check_directory(fd, private=True)
            self.dir_fd = fd
            fd = None
        except OSError:
            raise ProofError("credential_storage_error") from None
        finally:
            if fd is not None:
                os.close(fd)

    @staticmethod
    def _check_directory(fd, private=False):
        info = os.fstat(fd)
        mode = stat.S_IMODE(info.st_mode)
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid())
                or (mode & 0o022 and not info.st_mode & stat.S_ISVTX)
                or (private and (info.st_uid != os.getuid() or mode != 0o700))):
            raise ProofError("credential_storage_error")

    def close(self):
        os.close(self.dir_fd)

    def _open(self, name, flags, mode=0o600):
        fd = os.open(name, flags | os.O_NOFOLLOW | os.O_NONBLOCK, mode, dir_fd=self.dir_fd)
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1):
            os.close(fd)
            raise ProofError("credential_storage_error")
        return fd

    @contextlib.contextmanager
    def locked(self):
        fd = None
        try:
            fd = self._open("grant.lock", os.O_CREAT | os.O_RDWR)
            deadline = time.monotonic() + 30
            while True:
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        raise ProofError("credential_lock_timeout") from None
                    time.sleep(0.05)
            yield
        except OSError:
            raise ProofError("credential_storage_error") from None
        finally:
            if fd is not None:
                os.close(fd)

    def load(self):
        try:
            fd = self._open("grant.json", os.O_RDONLY)
        except FileNotFoundError:
            return {"schema": STORE_SCHEMA, "generation": None, "revision": 0,
                    "installation_id": str(uuid.uuid4()), "credential": None}
        try:
            with os.fdopen(fd, "rb") as handle:
                raw = handle.read(MAX_STORE + 1)
            if len(raw) > MAX_STORE:
                raise ProofError("credential_storage_error")
            value = json.loads(raw)
            if not isinstance(value, dict) or value.get("schema") != STORE_SCHEMA:
                raise ProofError("unknown_diagnostic_store_schema")
            identity(value["generation"])
            identity(value["installation_id"])
            if type(value["revision"]) is not int or value["revision"] < 0:
                raise ProofError("credential_storage_error")
            if value["credential"] is not None:
                self.grant(value)
            return value
        except (ValueError, KeyError, TypeError, OSError):
            raise ProofError("credential_storage_error") from None

    @staticmethod
    def grant(record):
        data = record["credential"]
        if data is None:
            raise ProofError("login_required")
        if (not isinstance(data, dict) or not nonempty(data.get("access_token"))
                or not data["access_token"].startswith("sk-ant-oat")
                or not nonempty(data.get("refresh_token"))
                or type(data.get("expires_at")) not in (float, int)
                or type(data.get("authorized_at")) not in (float, int)
                or not math.isfinite(data["expires_at"]) or not math.isfinite(data["authorized_at"])
                or not isinstance(data.get("scopes"), list)
                or not all(nonempty(s) for s in data["scopes"])
                or "user:inference" not in data["scopes"]):
            raise ProofError("credential_storage_error")
        return Grant(data["access_token"], data["refresh_token"], data["expires_at"],
                     data["account_id"], data["organization_id"], data["scopes"], data["authorized_at"])

    def write(self, record):
        encoded = json_bytes(record)
        if len(encoded) > MAX_STORE:
            raise ProofError("credential_storage_error")
        name = ".grant-" + secrets.token_hex(12)
        fd = None
        try:
            fd = self._open(name, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            with os.fdopen(fd, "wb") as handle:
                fd = None
                handle.write(encoded)
                handle.flush()
                os.fsync(handle.fileno())
            # Do not silently replace an unexpected target (including a symlink).
            try:
                target = self._open("grant.json", os.O_RDONLY)
            except FileNotFoundError:
                pass
            else:
                os.close(target)
            os.replace(name, "grant.json", src_dir_fd=self.dir_fd, dst_dir_fd=self.dir_fd)
            try:
                os.fsync(self.dir_fd)
            except OSError:
                # Replacement already happened: claiming the old record was
                # retained, or rolling a rotated refresh token back, is unsafe.
                raise ProofError("credential_commit_outcome_unknown") from None
        except OSError:
            raise ProofError("credential_storage_error") from None
        finally:
            if fd is not None:
                os.close(fd)
            try:
                os.unlink(name, dir_fd=self.dir_fd)
            except FileNotFoundError:
                pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, hdrs, newurl):
        return None


@contextlib.contextmanager
def network_deadline(seconds, kind):
    """Bound the CLI's main-thread HTTP work, including slow response headers.

    Socket timeouts alone reset on every successful read. POSIX's real timer
    also interrupts a provider that sends a byte before each socket timeout.
    Restore any caller timer/handler on exit; this helper runs only on the CLI
    main thread on its supported macOS/Linux platforms.
    """
    previous_handler = signal.getsignal(signal.SIGALRM)
    previous_timer = signal.getitimer(signal.ITIMER_REAL)
    started = time.monotonic()

    def expired(signum, frame):
        raise ProofError(kind)

    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, max(0.001, seconds))
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_handler)
        if previous_timer[0]:
            remaining = max(0.001, previous_timer[0] - (time.monotonic() - started))
            signal.setitimer(signal.ITIMER_REAL, remaining, previous_timer[1])


def check_environment():
    override = os.environ.get("ANTHROPIC_BASE_URL")
    if override and override.rstrip("/") != API_ORIGIN:
        raise ProofError("invalid_configuration")


class Transport:
    """One fixed HTTPS origin. No proxy, redirect, cookie, API-key or token env."""

    def __init__(self):
        check_environment()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    @contextlib.contextmanager
    def response(self, operation, token=None, body=None, session_id=None, model=None,
                 after=None, timeout=10, budget=None):
        paths = {"exchange": "/v1/oauth/token", "refresh": "/v1/oauth/token",
                 "bootstrap": "/api/claude_cli/bootstrap?entrypoint=cli&model=claude-opus-4-8",
                 "models": "/v1/models?limit=100", "messages": "/v1/messages"}
        if operation not in paths:
            raise ProofError("invalid_configuration")
        path = paths[operation]
        if operation == "models":
            if model:
                if not valid_model(model):
                    raise ProofError("invalid_model_response")
                path = "/v1/models/" + urllib.parse.quote(model, safe="")
            elif after:
                if not valid_model(after):
                    raise ProofError("invalid_model_response")
                path += "&after_id=" + urllib.parse.quote(after, safe="")
        request = urllib.request.Request(API_ORIGIN + path, data=body,
                                         headers=headers(operation, token, session_id),
                                         method="POST" if body is not None else "GET")
        try:
            kind = "partial_stream_error" if operation == "messages" else "transport_outcome_unknown"
            with network_deadline(timeout if budget is None else budget, kind):
                with self.opener.open(request, timeout=timeout) as response:
                    yield response
        except urllib.error.HTTPError as error:
            # Do not read or interpolate an error body: providers can echo secrets
            # and request content. Detailed classifications belong in Phase 1/2.
            status = error.code
            error.close()
            kind = {401: "login_expired", 403: "access_denied", 404: "endpoint_or_model_unavailable",
                    429: "provider_limited"}.get(status, "compatibility_error" if status < 500 else "auth_temporarily_unavailable")
            if operation in ("exchange", "refresh") and status == 400:
                kind = "login_required"
            raise ProofError(kind, status) from None
        except (OSError, urllib.error.URLError, TimeoutError):
            raise ProofError("transport_outcome_unknown") from None

    def json(self, operation, token=None, data=None, **kwargs):
        deadline = time.monotonic() + kwargs.get("timeout", 10)
        with self.response(operation, token, json_bytes(data) if data is not None else None, **kwargs) as response:
            raw = bytearray()
            while True:
                if time.monotonic() >= deadline:
                    raise ProofError("transport_outcome_unknown")
                chunk = response.read1(min(65536, MAX_BODY + 1 - len(raw)))
                raw.extend(chunk)
                if len(raw) > MAX_BODY:
                    raise ProofError("provider_payload_too_large")
                if not chunk:
                    break
        try:
            result = json.loads(raw)
        except (ValueError, UnicodeError):
            raise ProofError("invalid_provider_json") from None
        if not isinstance(result, dict):
            raise ProofError("invalid_provider_json")
        return result


class PendingLogin:
    def __init__(self, redirect_uri, clock=time.monotonic):
        self._clock = clock
        self.deadline = clock() + 300
        self.state = Secret(secrets.token_urlsafe(32))
        self.verifier = Secret(secrets.token_urlsafe(64))
        self.redirect_uri = redirect_uri
        self.claimed = False

    def authorization_url(self):
        challenge = base64.urlsafe_b64encode(hashlib.sha256(self.verifier.reveal().encode()).digest()).decode().rstrip("=")
        return AUTHORIZATION_URL + "?" + urllib.parse.urlencode({
            "client_id": CLIENT_ID, "response_type": "code", "redirect_uri": self.redirect_uri,
            "scope": " ".join(SCOPES), "code_challenge": challenge,
            "code_challenge_method": "S256", "state": self.state.reveal(), "code": "true",
        })

    def check(self):
        if self.claimed:
            raise ProofError("callback_replayed")
        if self._clock() >= self.deadline:
            raise ProofError("login_timeout")

    def parse(self, text):
        self.check()
        if not isinstance(text, str) or len(text.encode()) > MAX_CALLBACK:
            raise ProofError("invalid_callback")
        text = text.strip()
        state_values = []
        if "://" in text:
            try:
                parsed = urllib.parse.urlsplit(text)
                expected = urllib.parse.urlsplit(self.redirect_uri)
                if (parsed.scheme, parsed.netloc, parsed.path) != (expected.scheme, expected.netloc, expected.path) or parsed.fragment:
                    raise ProofError("invalid_callback")
                query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True, max_num_fields=12)
            except ValueError:
                raise ProofError("invalid_callback") from None
            if len(query.get("state", [])) != 1:
                raise ProofError("invalid_callback")
            state_values.extend(query["state"])
            if not secrets.compare_digest(query["state"][0].encode(), self.state.reveal().encode()):
                raise ProofError("callback_state_mismatch")
            if "error" in query:
                raise ProofError("login_denied")
            if len(query.get("code", [])) != 1:
                raise ProofError("invalid_callback")
            text = query["code"][0]
        if "#" in text:
            text, supplied_state = text.split("#", 1)
            state_values.append(supplied_state)
        if any(not secrets.compare_digest(s.encode(), self.state.reveal().encode()) for s in state_values):
            raise ProofError("callback_state_mismatch")
        if not nonempty(text) or text.startswith("sk-ant-") or any(c.isspace() for c in text):
            raise ProofError("invalid_callback")
        # A provider-issued bare code still uses this pending flow's PKCE/state.
        return Secret(text)

    def claim(self, code):
        self.check()
        self.claimed = True  # Consume before exchange, including on HTTP failure.
        return {"grant_type": "authorization_code", "client_id": CLIENT_ID,
                "code": code.reveal(), "state": self.state.reveal(),
                "redirect_uri": self.redirect_uri, "code_verifier": self.verifier.reveal()}


class Callbacks:
    def __init__(self, random_port=False):
        self.sockets = []
        self.fallback = random_port
        for attempt in range(10):
            self.sockets = []
            try:
                ipv4 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                self.sockets.append(ipv4)
                ipv4.bind(("127.0.0.1", 0 if self.fallback else 54545))
                port = ipv4.getsockname()[1]
                try:
                    ipv6 = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
                except OSError as error:
                    if error.errno not in (errno.EAFNOSUPPORT, errno.EPROTONOSUPPORT):
                        raise
                else:
                    self.sockets.append(ipv6)
                    ipv6.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
                    try:
                        ipv6.bind(("::1", port))
                    except OSError as error:
                        if error.errno != errno.EADDRNOTAVAIL:
                            raise
                        # Some Linux hosts disable IPv6 on loopback entirely.
                        ipv6.close()
                        self.sockets.remove(ipv6)
                for listener in self.sockets:
                    listener.listen(4)
                    listener.setblocking(False)
                self.redirect_uri = f"http://localhost:{port}/callback"
                return
            except OSError:
                self.close()
                self.fallback = True
        raise ProofError("callback_listener_unavailable")

    def close(self):
        for listener in self.sockets:
            listener.close()

    def accept(self, listener, pending):
        connection, _ = listener.accept()
        with connection:
            deadline = min(time.monotonic() + 1, pending.deadline)
            success = False
            code = None
            try:
                raw = bytearray()
                while b"\r\n\r\n" not in raw and len(raw) <= MAX_CALLBACK:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise ProofError("invalid_callback")
                    connection.settimeout(remaining)
                    chunk = connection.recv(min(2048, MAX_CALLBACK + 1 - len(raw)))
                    if not chunk:
                        break
                    raw.extend(chunk)
                if len(raw) > MAX_CALLBACK or b"\r\n\r\n" not in raw:
                    raise ProofError("invalid_callback")
                lines = raw.decode("ascii").split("\r\n")
                method, target, version = lines[0].split(" ")
                hosts = [line[5:].strip() for line in lines[1:] if line.lower().startswith("host:")]
                origin = urllib.parse.urlsplit(pending.redirect_uri)
                if (method != "GET" or version not in ("HTTP/1.0", "HTTP/1.1") or not target.startswith("/callback?")
                        or hosts != [origin.netloc]):
                    raise ProofError("invalid_callback")
                code = pending.parse("http://" + origin.netloc + target)
                success = True
            except (ProofError, ValueError, UnicodeError, OSError):
                pass
            message = b"Callback received. Return to the Mirror diagnostic terminal." if success else b"Callback rejected. Return to the diagnostic terminal."
            response = (b"HTTP/1.1 200 OK\r\n" if success else b"HTTP/1.1 400 Bad Request\r\n") + (
                b"Content-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\n"
                b"Referrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n"
                + f"Content-Length: {len(message)}\r\n\r\n".encode() + message)
            try:
                connection.settimeout(0.1)
                connection.sendall(response)
            except OSError:
                pass
            return code


def wait_for_callback(callbacks, pending, input_fd, notice, cancelled=lambda: False):
    # Darwin's KqueueSelector rejects an actual /dev/tty (EINVAL), although it
    # accepts the slave PTYs and pipes that our earlier tests exercised. This is
    # a fixed, tiny descriptor set; select supports both the terminal and sockets.
    with diagnostic_stage("callback_wait"), selectors.SelectSelector() as selector:
        for listener in callbacks.sockets:
            selector.register(listener, selectors.EVENT_READ, "callback")
        selector.register(input_fd, selectors.EVENT_READ, "manual")
        while True:
            pending.check()
            if cancelled():
                raise ProofError("login_cancelled")
            for key, _ in selector.select(min(0.2, max(0, pending.deadline - time.monotonic()))):
                if key.data == "callback":
                    code = callbacks.accept(key.fileobj, pending)
                    if code:
                        return code, "loopback"
                else:
                    raw = os.read(input_fd, MAX_CALLBACK + 1)
                    if not raw:
                        raise ProofError("login_cancelled")
                    try:
                        code = pending.parse(raw.decode())
                        return code, "manual"
                    except (ProofError, UnicodeError):
                        notice("Callback rejected; paste the final callback or code again (input hidden).\n")


def browser_login(callbacks, pending, no_browser):
    # The dedicated terminal is intentionally required: no URLs/codes on JSON
    # stdout, traces, environment, argv, or agent tool transcripts.
    with diagnostic_stage("terminal_open"):
        try:
            fd = os.open("/dev/tty", os.O_RDWR | os.O_NOCTTY)
        except OSError:
            raise ProofError("interactive_terminal_required") from None
    old = None
    try:
        with diagnostic_stage("terminal_setup"):
            old = termios.tcgetattr(fd)
            new = termios.tcgetattr(fd)
            new[3] &= ~termios.ECHO
            termios.tcflush(fd, termios.TCIFLUSH)
            termios.tcsetattr(fd, termios.TCSANOW, new)

        def notice(message):
            os.write(fd, message.encode())

        with diagnostic_stage("terminal_setup"):
            url = pending.authorization_url()
            notice("Mirror Phase 0: sign into your Claude subscription in this browser flow.\n"
                   "Authorization URL (only in this private terminal):\n" + url + "\n"
                   "Waiting up to five minutes. Paste callback/code here if needed; input is hidden. Ctrl-C cancels.\n")
            if not no_browser:
                try:
                    opened = webbrowser.open(url, new=2)
                except Exception:
                    opened = False
                if not opened:
                    notice("Browser launch failed; open the displayed URL manually.\n")
        return wait_for_callback(callbacks, pending, fd, notice)
    finally:
        with diagnostic_stage("terminal_cleanup"):
            try:
                # Never wait for terminal output to drain during cleanup.
                if old is not None:
                    try:
                        termios.tcflush(fd, termios.TCIFLUSH)
                    finally:
                        # A failed flush must not leave the user's shell with
                        # echo disabled; restoration is a separate obligation.
                        termios.tcsetattr(fd, termios.TCSANOW, old)
                os.write(fd, b"\n")
            finally:
                os.close(fd)


def login(store, transport, no_browser=False, random_port=False):
    with diagnostic_stage("credential_load"), store.locked():
        starting = store.load()
    with diagnostic_stage("callback_listener"):
        callbacks = Callbacks(random_port)
    try:
        pending = PendingLogin(callbacks.redirect_uri)
        code, mode = browser_login(callbacks, pending, no_browser)
        with diagnostic_stage("token_exchange"):
            data = transport.json("exchange", data=pending.claim(code))
        # Validate the credential kind before authorizing any bootstrap call.
        with diagnostic_stage("token_validation"):
            validation = dict(data)
            validation["account"] = {"uuid": "00000000-0000-4000-8000-000000000001"}
            validation["organization"] = {"uuid": "00000000-0000-4000-8000-000000000002"}
            Grant.from_token(validation)
            if not isinstance(data.get("account", {}), dict) or not isinstance(data.get("organization", {}), dict):
                raise ProofError("invalid_identity")
        # Empty or partial identity also requires bootstrap; never infer it from
        # another app or commit an incomplete grant.
        if not data.get("account", {}).get("uuid") or not data.get("organization", {}).get("uuid"):
            with diagnostic_stage("identity_bootstrap"):
                bootstrap = transport.json("bootstrap", Secret(data.get("access_token", "")))
                account = bootstrap.get("oauth_account", {})
                if not isinstance(account, dict):
                    raise ProofError("invalid_identity")
                data["account"] = {"uuid": data.get("account", {}).get("uuid") or account.get("account_uuid")}
                data["organization"] = {"uuid": data.get("organization", {}).get("uuid") or account.get("organization_uuid")}
        with diagnostic_stage("identity_validation"):
            grant = Grant.from_token(data)
        with diagnostic_stage("credential_commit"), store.locked():
            current = store.load()
            if current["generation"] != starting["generation"]:
                raise ProofError("newer_login_or_logout_won")
            store.write({"schema": STORE_SCHEMA, "generation": str(uuid.uuid4()), "revision": 1,
                         "installation_id": starting["installation_id"], "credential": grant.private_record()})
        return {"fresh_mirror_owned_grant": True, "callback_mode": mode,
                "callback_port": "random" if callbacks.fallback else "preferred", "expires_at": utc(grant.expires_at)}
    finally:
        with diagnostic_stage("callback_cleanup"):
            callbacks.close()


def refresh(store, transport):
    with store.locked():
        record = store.load()
        previous = store.grant(record)
        data = transport.json("refresh", data={"grant_type": "refresh_token", "client_id": CLIENT_ID,
                                              "refresh_token": previous.refresh.reveal()})
        grant = Grant.from_token(data, previous)
        record["credential"] = grant.private_record()
        record["revision"] += 1
        store.write(record)
        return {"real_token_endpoint_refresh": True, "revision": record["revision"],
                "replacement_refresh_token_returned": "refresh_token" in data,
                "expires_at": utc(grant.expires_at)}


def valid_model(value):
    import re
    return isinstance(value, str) and re.fullmatch(r"[a-zA-Z0-9_-]{1,128}", value) is not None


def model_summary(data):
    if not isinstance(data, dict) or not valid_model(data.get("id")):
        raise ProofError("invalid_model_response")
    result = {"id": data["id"]}
    # Omitted data stays unknown. Do not infer a context window from a name.
    for field in ("max_input_tokens", "max_tokens", "max_output_tokens"):
        if field in data and type(data[field]) is int and 0 < data[field] < 100_000_000:
            result[field] = data[field]
    return result


def models(transport, grant):
    deadline = time.monotonic() + 10
    result = []
    seen = set()
    cursor = None
    for _ in range(20):
        budget = deadline - time.monotonic()
        if budget <= 0:
            raise ProofError("metadata_timeout")
        page = transport.json("models", grant.access, after=cursor, timeout=budget)
        if not isinstance(page.get("data"), list) or type(page.get("has_more")) is not bool:
            raise ProofError("invalid_model_response")
        if len(page["data"]) > 100:
            raise ProofError("invalid_model_response")
        for item in page["data"]:
            summary = model_summary(item)
            if summary["id"] in seen:
                raise ProofError("invalid_model_response")
            seen.add(summary["id"])
            result.append(summary)
        if not page["has_more"]:
            return result
        cursor = page.get("last_id")
        if not valid_model(cursor) or cursor not in seen or not page["data"] or cursor != result[-1]["id"]:
            raise ProofError("invalid_model_response")
    raise ProofError("metadata_page_limit")


def parse_stream(lines, names):
    """Bounded SSE accumulator. No tool is executed until a complete message."""
    blocks = []
    partial = {}
    stopped = set()
    started = False
    ended = False
    reason = None
    payload = []
    total = 0

    def events():
        nonlocal total
        for line in lines:
            total += len(line)
            if total > MAX_BODY or len(line) > 65536:
                raise ProofError("partial_stream_error")
            if line.strip() == b"":
                if payload:
                    raw = b"\n".join(payload)
                    payload.clear()
                    try:
                        event = json.loads(raw)
                    except (ValueError, UnicodeError):
                        raise ProofError("partial_stream_error") from None
                    if not isinstance(event, dict):
                        raise ProofError("partial_stream_error")
                    yield event
            elif line.startswith(b"data:"):
                payload.append(line[5:].strip())

    try:
        for event in events():
            kind = event.get("type")
            if kind == "ping":
                continue
            if kind == "message_start" and not started:
                started = True
            elif kind == "content_block_start" and started:
                index = event.get("index")
                block = event.get("content_block")
                if type(index) is not int or index != len(blocks) or not isinstance(block, dict) or block.get("type") not in ("text", "thinking", "redacted_thinking", "tool_use"):
                    raise ProofError("partial_stream_error")
                if block["type"] == "tool_use":
                    block["name"] = names.decode(block.get("name"))
                    if not nonempty(block.get("id")):
                        raise ProofError("partial_stream_error")
                blocks.append(block)
            elif kind == "content_block_delta":
                index = event.get("index")
                if type(index) is not int or not 0 <= index < len(blocks) or index in stopped:
                    raise ProofError("partial_stream_error")
                block = blocks[index]
                delta = event.get("delta", {})
                pair = {"text_delta": ("text", "text"), "thinking_delta": ("thinking", "thinking"),
                        "signature_delta": ("thinking", "signature"), "input_json_delta": ("tool_use", "partial_json")}.get(delta.get("type"))
                if not pair or block["type"] != pair[0] or not isinstance(delta.get(pair[1]), str):
                    raise ProofError("partial_stream_error")
                if pair[1] == "partial_json":
                    partial[index] = partial.get(index, "") + delta[pair[1]]
                else:
                    block[pair[1]] = block.get(pair[1], "") + delta[pair[1]]
            elif kind == "content_block_stop":
                index = event.get("index")
                if type(index) is not int or not 0 <= index < len(blocks) or index in stopped:
                    raise ProofError("partial_stream_error")
                if index in partial:
                    blocks[index]["input"] = json.loads(partial[index])
                stopped.add(index)
            elif kind == "message_delta" and started:
                reason = event.get("delta", {}).get("stop_reason")
            elif kind == "message_stop" and started and reason and len(stopped) == len(blocks):
                ended = True
                break
            else:
                raise ProofError("partial_stream_error")
    except (ValueError, TypeError, KeyError, AttributeError, OSError):
        raise ProofError("partial_stream_error") from None
    if not ended or not blocks or reason not in ("end_turn", "tool_use"):
        raise ProofError("partial_stream_error")
    for block in blocks:
        # Opaque signed/redacted reasoning can exceed the credential field cap.
        # The entire SSE response is already bounded by MAX_BODY.
        if block["type"] == "thinking" and (not isinstance(block.get("signature"), str)
                or not block["signature"] or not isinstance(block.get("thinking"), str)):
            raise ProofError("unsigned_thinking")
        if block["type"] == "redacted_thinking" and (not isinstance(block.get("data"), str) or not block["data"]):
            raise ProofError("partial_stream_error")
    return blocks, reason


def complete(transport, grant, record, model, session_id, messages):
    if grant.expires_at - time.time() < 300:
        raise ProofError("refresh_required")
    body = request_body(model, messages, record["installation_id"], grant.account_id, session_id)
    deadline = time.monotonic() + 60
    with transport.response("messages", grant.access, body, session_id=session_id, timeout=30, budget=60) as response:
        def lines():
            buffered = bytearray()
            total = 0
            while True:
                if time.monotonic() >= deadline:
                    raise ProofError("partial_stream_error")
                # readline may make arbitrarily many successful socket reads
                # before a newline, defeating both socket and total timeouts.
                chunk = response.read1(65536)
                total += len(chunk)
                if total > MAX_BODY:
                    raise ProofError("partial_stream_error")
                if not chunk:
                    return
                buffered.extend(chunk)
                while b"\n" in buffered:
                    index = buffered.index(b"\n") + 1
                    yield bytes(buffered[:index])
                    del buffered[:index]
                if len(buffered) > 65536:
                    raise ProofError("partial_stream_error")

        return parse_stream(lines(), ToolNames(TOOLS))


def probe(store, transport, model):
    with store.locked():
        record = store.load()
        grant = store.grant(record)
        catalog = models(transport, grant)
        if model not in {row["id"] for row in catalog}:
            raise ProofError("model_unavailable")
        detail = model_summary(transport.json("models", grant.access, model=model))
        if detail["id"] != model:
            raise ProofError("invalid_model_response")
        session_id = str(uuid.uuid4())
        messages = [{"role": "user", "content": "For the Mirror protocol diagnostic, reply exactly MIRROR_PHASE0_TEXT_OK. Do not call a tool yet."}]
        first, reason = complete(transport, grant, record, model, session_id, messages)

        def text(blocks):
            return "".join(b.get("text", "") for b in blocks if b["type"] == "text").strip()

        if reason != "end_turn" or text(first) != "MIRROR_PHASE0_TEXT_OK" or any(b["type"] == "tool_use" for b in first):
            raise ProofError("text_probe_failed")
        messages += [{"role": "assistant", "content": first},
                     {"role": "user", "content": f'Call {ECHO_NAME} exactly once with value "{ECHO_VALUE}". After its result, reply exactly MIRROR_PHASE0_TOOL_OK.'}]
        second, reason = complete(transport, grant, record, model, session_id, messages)
        calls = [b for b in second if b["type"] == "tool_use"]
        if reason != "tool_use" or len(calls) != 1 or calls[0]["name"] != ECHO_NAME or calls[0].get("input") != {"value": ECHO_VALUE}:
            raise ProofError("tool_probe_failed")
        # Fixed echo only. No model-directed filesystem, shell, or app operation.
        messages += [{"role": "assistant", "content": second}, {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": calls[0]["id"], "content": ECHO_VALUE}]}]
        third, reason = complete(transport, grant, record, model, session_id, messages)
        if reason != "end_turn" or text(third) != "MIRROR_PHASE0_TOOL_OK" or any(b["type"] == "tool_use" for b in third):
            raise ProofError("tool_result_probe_failed")
        signed_replayed = sum(b["type"] == "thinking" for turn in (first, second) for b in turn)
        return {"model": model, "model_lookup": True, "model_detail": detail,
                "text": True, "tool_round_trip": True, "requests": 3,
                "signed_reasoning_blocks_replayed": signed_replayed,
                "reasoning_gate": "observed" if signed_replayed else "pending_no_signed_reasoning_observed"}


def status(store):
    with store.locked():
        record = store.load()
        if record["credential"] is None:
            return {"state": "logged_out", "source": "Mirror Phase 0 login"}
        grant = store.grant(record)
        return {"state": "usable" if grant.expires_at - time.time() >= 300 else "refresh_required",
                "source": "Mirror Phase 0 login", "account_id": grant.account_id,
                "organization_id": grant.organization_id, "revision": record["revision"],
                "expires_at": utc(grant.expires_at)}


def logout(store):
    with store.locked():
        record = store.load()
        record.update({"generation": str(uuid.uuid4()), "revision": 0, "credential": None})
        store.write(record)
    return {"local_logout": True}


def envelope(operation, result):
    try:
        revision = subprocess.check_output(["git", "-C", str(Path(__file__).resolve().parents[2]),
                                            "rev-parse", "HEAD"], stderr=subprocess.DEVNULL, text=True).strip()
    except (OSError, subprocess.CalledProcessError):
        revision = "unavailable"
    return {"schema_version": 1, "diagnostic": "claude-oauth-phase0", "operation": operation,
            "collected_at": utc(), "mirror_revision": revision,
            "compatibility_revision": PROFILE, "omp_revision": SOURCE_REVISION,
            "platform": sys.platform, "result": result}


class SafeParser(argparse.ArgumentParser):
    def error(self, message):
        raise ProofError("invalid_command_line")


def safe_error(error):
    recovery = {
        "invalid_command_line": "Run proof.py --help; never pass credentials or callback codes in arguments.",
        "invalid_configuration": "Unset a conflicting ANTHROPIC_BASE_URL and use only this diagnostic's fixed endpoints.",
        "credential_storage_error": "Use a fresh private diagnostic-only state directory; keep any existing grant private for manual recovery.",
        "credential_commit_outcome_unknown": "The complete new grant replaced the old file, but durable commit could not be confirmed. Inspect status; do not repeat the refresh blindly. Fix storage or establish a fresh login if needed.",
        "unknown_diagnostic_store_schema": "Use a fresh diagnostic-only state directory; do not point at another application's credentials.",
        "credential_lock_timeout": "Let the other diagnostic operation finish, then retry this command.",
        "login_required": "Run proof.py login yourself in a private terminal to establish a fresh Mirror-owned grant.",
        "login_expired": "Run proof.py refresh once; if rejected, run proof.py login in a private terminal.",
        "refresh_required": "Run proof.py refresh, then status, before another probe.",
        "interactive_terminal_required": "Run proof.py login yourself in a private terminal, outside agent tools and recordings.",
        "login_timeout": "Run proof.py login again in a private terminal and complete the new flow within five minutes.",
        "login_cancelled": "The pending flow was cancelled; run login again when ready.",
        "newer_login_or_logout_won": "A newer local login/logout won; inspect status before deciding whether to log in again.",
        "account_changed": "Stop and ask the manager to review the identity change; do not dispatch a probe.",
        "transport_outcome_unknown": "Do not automatically retry. A token exchange or refresh may have succeeded remotely; a fresh login may be required.",
        "partial_stream_error": "The response was incomplete or invalid. No automatic replay occurred; ask the manager to review this sanitized failure.",
        "provider_limited": "Stop this run and review the account's reported limit; do not switch accounts, models, or billing modes.",
    }.get(error.kind, "Stop the live run and give the manager this sanitized error and compatibility revision; keep provider payloads and credentials private.")
    stage = getattr(error, "stage", "command")
    classification = getattr(error, "classification", "controlled_error")
    if stage == "terminal_cleanup":
        recovery = "Terminal cleanup failed. If input remains hidden, run stty echo in that private terminal. Give the manager only this sanitized error before starting another login."
    return {"error": {"kind": error.kind, "http_status": error.status, "recovery": recovery,
                      "stage": stage if isinstance(stage, str) and stage in SAFE_STAGES else "command",
                      "error_class": classification if isinstance(classification, str) and classification in SAFE_ERROR_CLASSES else "unexpected_error"},
            "compatibility_revision": PROFILE}


def main():
    parser = SafeParser(description=__doc__)
    parser.add_argument("--state-dir", type=Path,
                        default=Path.home() / ".mirror" / "diagnostics" / "claude-oauth-phase0")
    commands = parser.add_subparsers(dest="command", required=True)
    login_parser = commands.add_parser("login", help="fresh Mirror-owned OAuth login in a private terminal")
    login_parser.add_argument("--no-browser", action="store_true")
    login_parser.add_argument("--random-port", action="store_true", help="test provider acceptance of a fallback loopback port")
    for name in ("status", "refresh", "models", "logout"):
        commands.add_parser(name)
    probe_parser = commands.add_parser("probe", help="three small subscription requests; fixed echo tool only")
    probe_parser.add_argument("--model", choices=tuple(MODELS), default="claude-opus-4-8")
    args = parser.parse_args()
    check_environment()
    store = Store(args.state_dir)
    try:
        if args.command == "status":
            result = status(store)
        elif args.command == "logout":
            result = logout(store)
        else:
            transport = Transport()
            if args.command == "login":
                result = login(store, transport, args.no_browser, args.random_port)
            elif args.command == "refresh":
                result = refresh(store, transport)
            elif args.command == "probe":
                result = probe(store, transport, args.model)
            else:
                with store.locked():
                    result = {"models": models(transport, store.grant(store.load()))}
        print(json.dumps(envelope(args.command, result), ensure_ascii=False))
    finally:
        store.close()


def run_cli():
    try:
        with diagnostic_stage("command"):
            main()
    except ProofError as error:
        print(json.dumps(safe_error(error)), file=sys.stderr)
        return 130 if error.kind == "login_cancelled" else 1
    return 0


if __name__ == "__main__":
    sys.exit(run_cli())
