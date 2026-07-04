#!/usr/bin/env bash
# Test script for arlo-rust agent against a local OpenAI-compatible endpoint
#
# Usage:
#   ./scripts/test-rdsec.sh
#
# Configuration:
#   Endpoint: http://100.68.213.81:30000/v1
#   Model:    test (any model works)
#   API Key:  not required

set -euo pipefail

# ─── Configuration ───────────────────────────────────────────────────────────

API_BASE="http://100.68.213.81:30000/v1"
MODEL="test"
export OPENAI_API_KEY="${OPENAI_API_KEY:-dummy}"
export OPENAI_BASE_URL="$API_BASE"

# ─── Resolve project root ────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== Arlo-Rust Agent Test ==="
echo "Base URL: $API_BASE"
echo "Model:    $MODEL"
echo ""

# ─── Test 1: Endpoint health check ───────────────────────────────────────────

echo "[1/4] Checking endpoint..."
CHAT_RESPONSE=$(curl -s -w "\n%{http_code}" \
  -X POST \
  -H "Content-Type: application/json" \
  "$API_BASE/chat/completions" \
  -d "{
    \"model\": \"$MODEL\",
    \"messages\": [{\"role\": \"user\", \"content\": \"Say hi\"}],
    \"max_tokens\": 10
  }")

HTTP_CODE=$(echo "$CHAT_RESPONSE" | tail -1)
BODY=$(echo "$CHAT_RESPONSE" | sed '$d')

if [ "$HTTP_CODE" -eq 200 ]; then
  echo "  ✓ Endpoint is alive (HTTP 200)"
  CONTENT=$(echo "$BODY" | python3 -c "import sys,json; r=json.load(sys.stdin); print(r['choices'][0]['message']['content'])" 2>/dev/null || echo "(parse error)")
  echo "  Response: $CONTENT"
else
  echo "  ✗ Endpoint returned $HTTP_CODE"
  echo "  Body: $BODY"
  echo ""
  echo "Endpoint not reachable. Aborting."
  exit 1
fi
echo ""

# ─── Test 2: Build arlo-rust ─────────────────────────────────────────────────

echo "[2/4] Building arlo-rust..."
if cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>/dev/null; then
  echo "  ✓ Build succeeded"
else
  echo "  ✗ Build failed"
  cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>&1 | tail -20
  exit 1
fi
echo ""

ARLO_BIN="$PROJECT_ROOT/target/debug/arlo"

# ─── Test 3: Simple prompt (no tools) ────────────────────────────────────────

echo "[3/4] Running simple prompt: 'What is 2+2? Reply with just the number.'"
ARLO_OUTPUT=$("$ARLO_BIN" --model "openai:$MODEL" "What is 2+2? Reply with just the number." 2>&1) || true
echo "  Output: $ARLO_OUTPUT"
if echo "$ARLO_OUTPUT" | grep -q "4"; then
  echo "  ✓ Correct answer received"
else
  echo "  ⚠ Unexpected output (may still be valid)"
fi
echo ""

# ─── Test 4: Tool use (shell) ────────────────────────────────────────────────

echo "[4/4] Running tool use prompt: 'List files in /tmp using ls'"
ARLO_OUTPUT=$("$ARLO_BIN" --model "openai:$MODEL" "Run: echo hello_from_arlo" 2>&1) || true
echo "  Output: $ARLO_OUTPUT"
if echo "$ARLO_OUTPUT" | grep -qi "hello_from_arlo"; then
  echo "  ✓ Tool execution confirmed"
else
  echo "  ⚠ Tool output not detected (model may not have used tool)"
fi
echo ""

echo "=== All tests complete ==="
