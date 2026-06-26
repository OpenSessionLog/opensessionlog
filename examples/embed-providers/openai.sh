#!/usr/bin/env bash
# OpenSessionLog embedder provider: OpenAI text-embedding-3-small
#
# Usage:
#   export OPENAI_API_KEY="sk-..."
#   osl embed --provider ./examples/embed-providers/openai.sh
#
# Requires: curl, jq

set -euo pipefail

MODEL="text-embedding-3-small"
MODEL_DIMENSIONS=1536

if [ -z "${OPENAI_API_KEY:-}" ]; then
    echo >&2 "error: OPENAI_API_KEY is not set"
    exit 1
fi

# Declare the model header (must be first stdout line, before reading stdin)
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

# Build parallel arrays of ids and texts
IDS=()
TEXTS=()
for entry in "${INPUTS[@]}"; do
    IDS+=("$(echo "$entry" | jq -r '.id')")
    TEXTS+=("$(echo "$entry" | jq -r '.text')")
done

# Build JSON array of text strings for the API
JSON_ARRAY="["
SEP=""
for t in "${TEXTS[@]}"; do
    JSON_ARRAY+="$SEP$(echo "$t" | jq -Rs .)"
    SEP=","
done
JSON_ARRAY+="]"

# Call the OpenAI embeddings API
RESPONSE=$(curl -s -f https://api.openai.com/v1/embeddings \
    -H "Authorization: Bearer $OPENAI_API_KEY" \
    -H "Content-Type: application/json" \
    -d "{\"input\":$JSON_ARRAY,\"model\":\"$MODEL\"}")

# Output one result line per input, preserving order via index
echo "$RESPONSE" | jq -c '
    .data | sort_by(.index)[] | {id: $ids[.index], embedding: .embedding}
' --argjson ids "$(printf '%s\n' "${IDS[@]}" | jq -R . | jq -s .)"
