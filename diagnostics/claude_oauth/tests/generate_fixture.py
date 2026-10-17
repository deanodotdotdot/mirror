"""Regenerate synthetic wire fixture using an independent Bun >=1.4 oracle.

Usage: python3 diagnostics/claude_oauth/tests/generate_fixture.py /path/to/bun
Normal tests have no Bun dependency. Never supply a live request or credential.
"""

import base64
import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import compatibility as c

ACCOUNT = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
INSTALLATION = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
SESSION = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"


def main(bun):
    version = subprocess.check_output([bun, "--version"], text=True).strip()
    if tuple(int(v) for v in version.split(".")[:2]) < (1, 4):
        raise SystemExit("Use Bun >=1.4, matching the pinned OMP packageManager requirement.")
    tools = [{"name": "read", "description": "Read diagnostic fixture", "input_schema": {"type": "object"}},
             {"name": "_read", "description": "Existing prefix stays reversible", "input_schema": {"type": "object"}}]
    messages = [
        {"role": "user", "content": "🪞 π Unicode prompt: cch=00000 remains user content."},
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "fixture reasoning", "signature": "FIXTURE_SIGNATURE_UNCHANGED"},
            {"type": "redacted_thinking", "data": "FIXTURE_OPAQUE_UNCHANGED"},
            {"type": "tool_use", "name": "_read", "id": "FIXTURE_TOOL_ID", "input": {}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "FIXTURE_TOOL_ID", "content": "π fixture result"}]},
    ]
    body = c.request_body("claude-opus-4-8", messages, INSTALLATION, ACCOUNT, SESSION,
                          tools=tools, tool_choice={"type": "tool", "name": "read"})
    value = json.loads(body)
    billing = value["system"][0]["text"]
    patched = billing.split("cch=")[1][:5]
    value["system"][0]["text"] = billing.replace("cch=" + patched, "cch=00000")
    unpatched = c.json_bytes(value)
    script = 'const b=Buffer.from(process.argv[1],"base64"); process.stdout.write(Bun.hash.xxHash64(b, BigInt(process.argv[2])).toString(16).padStart(16,"0"));'

    def bun_hash(data, seed):
        return subprocess.check_output([bun, "-e", script, base64.b64encode(data).decode(), str(seed)], text=True).strip()

    expected = bun_hash(unpatched, 0x4d659218e32a3268)
    assert patched == f"{int(expected, 16) & 0xfffff:05x}"
    seed_script = 'const s=process.argv[1]; const k=[4,7,20].map(i=>s[i]??"0").join(""); process.stdout.write(Bun.SHA256.hash("59cf53e54c78"+k+"2.1.280","hex").slice(0,3));'
    suffix = subprocess.check_output([bun, "-e", seed_script, messages[0]["content"]], text=True).strip()
    assert billing.startswith("x-anthropic-billing-header: cc_version=2.1.280." + suffix)
    vectors = []
    for data, seed in [(b"", 0), (b"hello", 0), (bytes(range(256)), 0),
                       (bytes(range(256)), 0x4d659218e32a3268), (unpatched, 0x4d659218e32a3268)]:
        vectors.append({"input_hex": data.hex(), "seed": seed, "expected": bun_hash(data, seed)})
    fixture = {"profile": c.PROFILE, "source_revision": c.SOURCE_REVISION,
               "oracle": f"Bun {version} SHA256 + Bun.hash.xxHash64; synthetic inputs only",
               "model": "claude-opus-4-8", "canonical_messages": messages, "tools": tools,
               "tool_choice": {"type": "tool", "name": "read"}, "request_utf8": body.decode(),
               "xxhash64_bun_vectors": vectors}
    (Path(__file__).parent / "fixtures" / "wire.json").write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n")
    print(f"Verified synthetic CCH and UTF-16 seed with Bun {version}; {len(body)} serialized bytes.")


if __name__ == "__main__":
    main(sys.argv[1])
