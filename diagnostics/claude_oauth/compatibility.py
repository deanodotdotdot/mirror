"""Frozen Phase 0 wire profile; not a production provider.

Adapted from oh-my-pi df4f996e66a200ab7e0496114d43bdf85f32194d.
See README.md for source paths and LICENSE-OMP for the upstream MIT notice.
"""

import copy
import hashlib
import json
import platform
import re

SOURCE_REVISION = "df4f996e66a200ab7e0496114d43bdf85f32194d"
PROFILE = "mirror-phase0-omp-df4f996-v1"
CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"  # Public protocol metadata.
CLI_VERSION = "2.1.280"
SDK_VERSION = "0.112.1"
SCOPES = (
    "org:create_api_key", "user:profile", "user:inference",
    "user:sessions:claude_code", "user:mcp_servers", "user:file_upload",
)
AUTHORIZATION_URL = "https://claude.ai/oauth/authorize"
API_ORIGIN = "https://api.anthropic.com"
OAUTH_BETA = "oauth-2025-04-20"
BETAS = (
    "claude-code-20250219", OAUTH_BETA, "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13", "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05", "mid-conversation-system-2026-04-07",
    "effort-2025-11-24", "fallback-credit-2026-06-01",
    "fine-grained-tool-streaming-2025-05-14",
)
IDENTITY_TEXT = "You are Claude Code, Anthropic's official CLI for Claude."
# Explicit candidates from the pinned catalog. A listing is not an entitlement,
# and a fixture is not proof that the server accepted a model or its reasoning.
MODELS = {
    "claude-opus-4-8": {"type": "adaptive", "display": "summarized"},
    "claude-sonnet-4-6": {"type": "adaptive"},
    "claude-sonnet-5": {"type": "adaptive", "display": "summarized"},
}
ECHO_NAME = "mirror_diagnostic_echo"
ECHO_VALUE = "Mirror π 🪞 phase0"
SYSTEM_TEXT = "Run only the requested small Mirror protocol diagnostic."
TOOLS = [{
    "name": ECHO_NAME,
    "description": "Echo the exact diagnostic value. No filesystem or command access.",
    "input_schema": {
        "type": "object", "properties": {"value": {"type": "string"}},
        "required": ["value"], "additionalProperties": False,
    },
}]


class ProofError(Exception):
    """Only a fixed kind and optional numeric status may cross the CLI boundary."""

    def __init__(self, kind, status=None):
        self.kind = kind
        self.status = status
        super().__init__(kind)


def json_bytes(value):
    # Matches JSON.stringify's compact UTF-8 convention for this profile's types.
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()


def xxhash64(data, seed=0):
    """XXH64 reference algorithm, modulo 2**64; fixture-checked against Bun."""
    mask = (1 << 64) - 1
    p1, p2, p3, p4, p5 = (11400714785074694791, 14029467366897019727,
                          1609587929392839161, 9650029242287828579, 2870177450012600261)

    def rol(value, count):
        value &= mask
        return ((value << count) | (value >> (64 - count))) & mask

    def rnd(acc, value):
        return (rol(acc + value * p2, 31) * p1) & mask

    def word(offset, size):
        return int.from_bytes(data[offset:offset + size], "little")

    offset = 0
    if len(data) >= 32:
        lanes = [(seed + p1 + p2) & mask, (seed + p2) & mask, seed & mask, (seed - p1) & mask]
        while offset <= len(data) - 32:
            lanes = [rnd(lanes[i], word(offset + 8 * i, 8)) for i in range(4)]
            offset += 32
        h = sum(rol(lanes[i], (1, 7, 12, 18)[i]) for i in range(4)) & mask
        for lane in lanes:
            h = ((h ^ rnd(0, lane)) * p1 + p4) & mask
    else:
        h = (seed + p5) & mask
    h = (h + len(data)) & mask
    while offset <= len(data) - 8:
        h = (rol(h ^ rnd(0, word(offset, 8)), 27) * p1 + p4) & mask
        offset += 8
    if offset <= len(data) - 4:
        h = (rol(h ^ (word(offset, 4) * p1), 23) * p2 + p3) & mask
        offset += 4
    while offset < len(data):
        h = (rol(h ^ (data[offset] * p5), 11) * p1) & mask
        offset += 1
    h ^= h >> 33
    h = (h * p2) & mask
    h ^= h >> 29
    h = (h * p3) & mask
    return (h ^ (h >> 32)) & mask


class ToolNames:
    def __init__(self, tools):
        self.forward = {}
        self.reverse = {}
        for tool in tools:
            name = tool.get("name")
            if not isinstance(name, str) or not re.fullmatch(r"[a-zA-Z0-9_-]{1,63}", name):
                raise ProofError("invalid_tool_name")
            wire = "_" + name
            if name in self.forward or wire in self.reverse:
                raise ProofError("tool_name_collision")
            self.forward[name] = wire
            self.reverse[wire] = name

    def encode(self, name):
        if name not in self.forward:
            raise ProofError("unknown_tool_name")
        return self.forward[name]

    def decode(self, name):
        if name not in self.reverse:
            raise ProofError("unknown_tool_name")
        return self.reverse[name]


def headers(operation, token=None, session_id=None):
    result = {"Content-Type": "application/json", "Accept": "application/json",
              "Accept-Encoding": "identity"}
    if token is not None:
        result["Authorization"] = "Bearer " + token.reveal()
    if operation == "exchange":
        return result
    if operation == "refresh":
        result.update({"anthropic-beta": OAUTH_BETA,
                       "User-Agent": f"anthropic-sdk-typescript/{SDK_VERSION} userOAuthProvider"})
        return result
    result.update({"anthropic-beta": OAUTH_BETA,
                   "User-Agent": f"claude-cli/{CLI_VERSION} (external, cli)"})
    if operation == "bootstrap":
        result["Accept"] = "application/json, text/plain, */*"
        result["User-Agent"] = f"claude-code/{CLI_VERSION}"
    if operation in ("models", "messages"):
        result["anthropic-version"] = "2023-06-01"
    if operation == "messages":
        result.update({
            "Accept": "text/event-stream", "anthropic-beta": ",".join(BETAS),
            "anthropic-dangerous-direct-browser-access": "true", "x-app": "cli",
            "X-Claude-Code-Session-Id": session_id,
            "X-Stainless-Arch": {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x64"}.get(platform.machine(), "unknown"),
            "X-Stainless-OS": {"Darwin": "MacOS", "Linux": "Linux"}.get(platform.system(), "Unknown"),
            "X-Stainless-Lang": "js", "X-Stainless-Package-Version": SDK_VERSION,
            "X-Stainless-Retry-Count": "0", "X-Stainless-Runtime": "node",
            "X-Stainless-Runtime-Version": "v26.3.0", "X-Stainless-Timeout": "600",
        })
    return result


def request_body(model, messages, installation_id, account_id, session_id,
                 tools=TOOLS, tool_choice=None, extra_body=None):
    if model not in MODELS:
        raise ProofError("unreviewed_model")
    # No extensible caller options in the spike: these must not replace transport
    # identity, billing, cache boundaries, stream mode, tools, or reasoning.
    if extra_body:
        raise ProofError("conflicting_extra_body")
    names = ToolNames(tools)
    wire_messages = copy.deepcopy(messages)
    for message in wire_messages:
        if isinstance(message["content"], list):
            for block in message["content"]:
                if block.get("type") == "tool_use":
                    block["name"] = names.encode(block["name"])
    first = next((m["content"] for m in messages if m["role"] == "user"), "")
    if not isinstance(first, str):
        first = "".join(b.get("text", "") for b in first if b.get("type") == "text")
    # JS indexes UTF-16 code units, not Unicode scalar values.
    units = first.encode("utf-16-le", errors="surrogatepass")
    k = "".join(units[2*i:2*i+2].decode("utf-16-le", errors="surrogatepass")
                if len(units) >= 2*i+2 else "0" for i in (4, 7, 20))
    # Bun/TextEncoder replaces isolated surrogate code units with U+FFFD.
    seed_text = ("59cf53e54c78" + k + CLI_VERSION).encode("utf-16-le", errors="surrogatepass").decode("utf-16-le", errors="replace")
    suffix = hashlib.sha256(seed_text.encode()).hexdigest()[:3]
    billing = f"x-anthropic-billing-header: cc_version={CLI_VERSION}.{suffix}; cc_entrypoint=cli; cch=00000;"
    system = [{"type": "text", "text": billing},
              {"type": "text", "text": IDENTITY_TEXT},
              {"type": "text", "text": SYSTEM_TEXT, "cache_control": {"type": "ephemeral"}}]
    wire_tools = copy.deepcopy(tools)
    for tool in wire_tools:
        tool["name"] = names.encode(tool["name"])
    if wire_tools:
        wire_tools[-1]["cache_control"] = {"type": "ephemeral"}
    device = hashlib.sha256(("mirror-claude-device-id-v1\0" + installation_id + "\0" + account_id).encode()).hexdigest()
    body = {"model": model, "messages": wire_messages, "system": system, "tools": wire_tools,
            "metadata": {"user_id": json_bytes({"device_id": device, "session_id": session_id,
                                                "account_uuid": account_id}).decode()},
            "max_tokens": 2048, "thinking": dict(MODELS[model]),
            "context_management": {"edits": [{"type": "clear_thinking_20251015", "keep": "all"}]},
            "output_config": {"effort": "low"}, "stream": True}
    if tool_choice:
        if tool_choice.get("type") != "tool" or set(tool_choice) != {"type", "name"}:
            raise ProofError("invalid_tool_choice")
        body["tool_choice"] = {"type": "tool", "name": names.encode(tool_choice["name"])}
    encoded = json_bytes(body)
    # Compute the patch offset from the known structural prefix. Never search or
    # replace user text, even when it contains a byte-for-byte billing marker.
    prefix = json_bytes({"model": model, "messages": wire_messages})[:-1]
    billing_prefix = b',"system":[{"type":"text","text":"' + billing.encode().split(b"cch=")[0] + b"cch="
    offset = len(prefix) + len(billing_prefix)
    if encoded[offset:offset + 5] != b"00000":
        raise ProofError("fingerprint_layout_changed")
    attestation = f"{xxhash64(encoded, 0x4d659218e32a3268) & 0xfffff:05x}".encode()
    return encoded[:offset] + attestation + encoded[offset + 5:]
