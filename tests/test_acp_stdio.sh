#!/bin/bash
# Test script for aginx ACP stdio mode
# Simulates IDE sending ACP requests

echo "=== Testing aginx ACP stdio mode ==="
echo ""

# Build first
echo "Building aginx..."
cargo build --manifest-path /Users/sophiehe/Documents/yinnhoos/aginx/Cargo.toml --bin aginx 2>/dev/null

AGINX_BIN="/Users/sophiehe/Documents/yinnhoos/aginx/target/debug/aginx"

PASS=0
FAIL=0

# Test 1: Initialize
echo "Test 1: initialize"
RESULT=$(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true},"clientInfo":{"name":"test-client","version":"1.0.0"}}}' | \
    $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"protocolVersion":"0.15.0"'; then
    echo "✓ PASS: initialize response contains protocolVersion"
    ((PASS++))
else
    echo "✗ FAIL: initialize response missing protocolVersion"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 2: newSession (with claude agent - supports sessions)
echo "Test 2: newSession (claude agent)"
RESULT=$(
(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":2,"method":"newSession","params":{"cwd":"/tmp","_meta":{"agentId":"claude"}}}'
) | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"sessionId"'; then
    echo "✓ PASS: newSession created a session"
    ((PASS++))
else
    echo "✗ FAIL: newSession did not create a session"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 3: listSessions
echo "Test 3: listSessions"
RESULT=$(
(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":2,"method":"listSessions","params":{}}'
) | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"sessions"'; then
    echo "✓ PASS: listSessions returned sessions array"
    ((PASS++))
else
    echo "✗ FAIL: listSessions did not return sessions"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 4: Unknown method (should return error)
echo "Test 4: unknown method (should return error)"
RESULT=$(echo '{"jsonrpc":"2.0","id":1,"method":"unknownMethod","params":{}}' | \
    $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"code":-32601'; then
    echo "✓ PASS: unknown method returns error -32601"
    ((PASS++))
else
    echo "✗ FAIL: unknown method did not return proper error"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 5: Invalid JSON (should return parse error)
echo "Test 5: invalid JSON"
RESULT=$(echo 'not valid json' | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"code":-32700'; then
    echo "✓ PASS: invalid JSON returns error -32700"
    ((PASS++))
else
    echo "✗ FAIL: invalid JSON did not return parse error"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 6: prompt with non-existent session (tests streaming path error handling)
echo "Test 6: prompt with invalid session (streaming error handling)"
RESULT=$(
(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":2,"method":"prompt","params":{"sessionId":"invalid-session","prompt":[{"type":"text","text":"hello"}]}}'
) | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"code":-32603'; then
    echo "✓ PASS: prompt with invalid session returns error -32603"
    ((PASS++))
else
    echo "✗ FAIL: prompt did not return proper error"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 7: prompt with empty text (validation)
echo "Test 7: prompt with empty text"
RESULT=$(
(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":2,"method":"newSession","params":{"cwd":"/tmp","_meta":{"agentId":"claude"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":3,"method":"prompt","params":{"sessionId":"sess_test","prompt":[]}}'
) | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"code":-32602'; then
    echo "✓ PASS: prompt with empty text returns error -32602"
    ((PASS++))
else
    echo "✗ FAIL: prompt with empty text did not return proper error"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 8: permissionResponse with missing session (Phase 3)
echo "Test 8: permissionResponse with invalid session"
RESULT=$(
(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":2,"method":"permissionResponse","params":{"sessionId":"invalid-session","outcome":{"outcome":"selected","optionId":"opt_0"}}}'
) | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"code":-32603'; then
    echo "✓ PASS: permissionResponse with invalid session returns error -32603"
    ((PASS++))
else
    echo "✗ FAIL: permissionResponse did not return proper error"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

# Test 9: permissionResponse with missing outcome (validation)
echo "Test 9: permissionResponse with missing outcome"
RESULT=$(
(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
sleep 0.1
echo '{"jsonrpc":"2.0","id":2,"method":"permissionResponse","params":{"sessionId":"sess_test"}}'
) | $AGINX_BIN acp --stdio 2>/dev/null)

if echo "$RESULT" | grep -q '"code":-32602'; then
    echo "✓ PASS: permissionResponse with missing outcome returns error -32602"
    ((PASS++))
else
    echo "✗ FAIL: permissionResponse did not return proper validation error"
    echo "$RESULT"
    ((FAIL++))
fi
echo ""

echo "=== Test Results ==="
echo "Passed: $PASS"
echo "Failed: $FAIL"
if [ $FAIL -eq 0 ]; then
    echo "All tests passed!"
    exit 0
else
    exit 1
fi
