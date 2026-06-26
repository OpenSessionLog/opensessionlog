#!/usr/bin/env bash
# OpenSessionLog embedder provider: Local llamafile embedding endpoint
#
# Usage:
#   1. Start a llamafile server:
#      llamafile --server --embedding --port 8080
#
#   2. Run osl embed:
#      osl embed --provider ./examples/embed-providers/llamafile.sh
#
# Environment variables:
#   LLAMAFILE_HOST  (default: http://localhost:8080)
#   LLAMAFILE_MODEL (default: default model loaded in the server)
#
# Requires: curl, jq

set -euo pipefail

HOST="${LLAMAFILE_HOST:-http://localhost:8080}"
MODEL="${LLAMAFILE_MODEL:-default}"
MODEL_DIMENSIONS=4096  # adjust to match your model's output dimension

# Declare the model header (first stdout line, before reading stdin)
echo "{\"type\":\"model\",\"model\":\"$MODEL\",\"dimensions\":$MODEL_DIMENSIONS}"

# Read all input lines
INPUTS=()
while IFS= read -r line; do
    [ -z "$line" ] && continue
    INPUTS+=("$line")
done

# No messages → no-op
if [ ${#INPUTS[@]} -eq 0 ]; then
    exit 0
fi

# Process each message and emit results inline
for entry in "${INPUTS[@]}"; do
    ID=$(echo "$entry" | jq -r '.id')
    TEXT=$(echo "$entry" | jq -r '.text')

    RESPONSE=$(curl -s -f "$HOST/v1/embeddings" \
        -H "Content-Type: application/json" \
        -d "$(jq -n --arg input "$TEXT" --arg model "$MODEL" \
            '{input: $input, model: $model}')")

    EMBEDDING=$(echo "$RESPONSE" | jq -c '.data[0].embedding')
    echo "{\"id\":\"$ID\",\"embedding\":$EMBEDDING}"
done
