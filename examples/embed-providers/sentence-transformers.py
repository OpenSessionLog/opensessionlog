#!/usr/bin/env python3
"""OpenSessionLog embedder provider: sentence-transformers (local).

Uses a local sentence-transformers model to compute embeddings.
The model downloads on first run (~500 MB for all-MiniLM-L6-v2).

Usage:
    pip install sentence-transformers
    osl embed --provider ./examples/embed-providers/sentence-transformers.py

Environment variables:
    ST_MODEL   Model name (default: all-MiniLM-L6-v2, 384 dims)
"""
import json
import os
import sys

MODEL_NAME = os.environ.get("ST_MODEL", "all-MiniLM-L6-v2")
MODEL_DIMENSIONS = 384  # all-MiniLM-L6-v2; change if using a different model

# Lazy-load the model so the header is emitted quickly
_model = None


def get_model():
    global _model
    if _model is None:
        from sentence_transformers import SentenceTransformer
        _model = SentenceTransformer(MODEL_NAME)
    return _model


def main():
    # Header line: must be emitted before reading stdin
    print(json.dumps({
        "type": "model",
        "model": MODEL_NAME,
        "dimensions": MODEL_DIMENSIONS,
    }), flush=True)

    inputs: list[dict] = []
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        inputs.append(json.loads(line))

    if not inputs:
        return  # no-op; header already sent

    # Extract texts in order
    texts = [obj["text"] for obj in inputs]

    # Compute embeddings (batched automatically by sentence-transformers)
    model = get_model()
    embeddings = model.encode(texts, show_progress_bar=False)

    # Emit one result per input
    for obj, emb in zip(inputs, embeddings):
        print(json.dumps({
            "id": obj["id"],
            "embedding": emb.tolist(),
        }))


if __name__ == "__main__":
    main()
