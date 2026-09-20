#!/usr/bin/python3
"""Deterministic peer for the documented CLI structured-output contract."""
import json
import os
import sys

if sys.argv[1:] == ["--version"]:
    print("2.1.223 (Claude Code)")
    sys.exit(0)
args = sys.argv[1:]
assert args[args.index("--tools") + 1] == ""
assert args[args.index("--disallowedTools") + 1] == "mcp__*"
assert "--safe-mode" in args and "--no-chrome" in args
assert "--no-session-persistence" in args
assert "--dangerously-skip-permissions" not in args
schema = json.loads(args[args.index("--json-schema") + 1])
assert set(schema["required"]) == {"content", "tool_calls"}
request = json.load(sys.stdin)
assert request["tools"][0]["name"] == "read_file"
assert request["tools"][0]["input_schema"] == {"type": "object"}
messages = request["messages"]
if messages[-1]["role"] == "tool":
    assert messages[-1]["tool_call_id"] == "call-1"
    assert messages[-2]["tool_calls"][0]["arguments"] == {"path": "test.txt"}
    output = {"content": "Read: " + messages[-1]["content"], "tool_calls": []}
else:
    output = {"content": "", "tool_calls": [{"id": "call-1", "name": "read_file", "arguments": {"path": "test.txt"}}]}
scenario = os.environ.get("RYNNA_TEST_SCENARIO", "")
if scenario == "reused":
    output = {"content": "", "tool_calls": [{"id": "call-1", "name": "read_file", "arguments": {"path": "test.txt"}}]}
if scenario == "native":
    print(json.dumps({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "bad", "name": "Bash", "input": {"command": "uname"}}]}}))
if scenario == "native-stream":
    print(json.dumps({"type": "stream_event", "event": {"type": "content_block_start", "content_block": {"type": "tool_use", "name": "Bash"}}}))
if scenario == "missing":
    print(json.dumps({"type": "result", "subtype": "success"}))
    sys.exit(0)
if scenario == "duplicate":
    print(json.dumps({"type": "result", "subtype": "success", "structured_output": output}))
# Intermediate prose is NOT the final structured response.
print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "internal planning"}]}}))
print(json.dumps({"type": "result", "subtype": "success", "structured_output": output}))
