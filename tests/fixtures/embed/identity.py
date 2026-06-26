#!/usr/bin/env python3
"""Deterministic 8-dim fixture embedder for OpenSessionLog tests.

Reads NDJSON lines from stdin, emits a model header first, then one result
line per input. The header is emitted even when stdin is empty.
"""
import hashlib
import json
import sys


def embed(text: str) -> list[float]:
    digest = hashlib.sha256(text.encode("utf-8")).digest()
    vals = []
    for i in range(8):
        u32 = int.from_bytes(digest[i * 4 : (i + 1) * 4], "little")
        vals.append(u32 / 0xFFFFFFFF)
    return vals


def main() -> None:
    print(
        json.dumps({"type": "model", "model": "identity-fixture", "dimensions": 8}),
        flush=True,
    )
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        obj = json.loads(line)
        result = {"id": obj["id"], "embedding": embed(obj["text"])}
        print(json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
