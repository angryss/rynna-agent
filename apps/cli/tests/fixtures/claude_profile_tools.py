#!/usr/bin/python3
"""Offline Claude CLI peer exercising real CLI profile composition."""
import json
import sys

if sys.argv[1:] == ["--version"]:
    print("2.1.223 (Claude Code)")
    sys.exit(0)
args = sys.argv[1:]
assert "--safe-mode" in args
assert args[args.index("--tools") + 1] == ""
assert args[args.index("--disallowedTools") + 1] == "mcp__*"
text = sys.stdin.read()
if "--json-schema" not in args:
    result = "no-tools"
    print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": result}]}}))
    print(json.dumps({"type": "result", "subtype": "success", "result": result}))
    sys.exit(0)
request = json.loads(text)
names = sorted(tool["name"] for tool in request["tools"])
messages = request["messages"]
prompt = next(m["content"] for m in reversed(messages) if m["role"] == "user")
if prompt in {"inspect", "inspect-denied"} and messages[-1]["role"] != "tool":
    output = {"content": "", "tool_calls": [{"id": "inspect-1", "name": "run_command", "arguments": {"program": "inspect_os" if prompt == "inspect" else "unauthorized_shell"}}]}
else:
    output = {"content": messages[-1]["content"] if messages[-1]["role"] == "tool" else ",".join(names), "tool_calls": []}
print(json.dumps({"type": "result", "subtype": "success", "structured_output": output}))
