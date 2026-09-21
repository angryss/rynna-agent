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
if prompt in {"read-default", "search-default", "write-default"} and messages[-1]["role"] != "tool":
    operation, arguments = {
        "read-default": ("read_file", {"path":"sample.txt"}),
        "search-default": ("search_files", {"path":".", "pattern":"default-file-fixture"}),
        "write-default": ("write_file", {"path":"never-created", "content":"must not write"}),
    }[prompt]
    output = {"content":"", "tool_calls":[{"id":"default-1","name":operation,"arguments":arguments}]}
elif prompt == "yolo-command" and messages[-1]["role"] != "tool":
    assert "write_file" in names and "run_command" in names and "read_file" in names
    output = {"content":"", "tool_calls":[{"id":"yolo-1","name":"run_command","arguments":{"program":"/bin/sh","arguments":["-c","printf yolo-native-result"]}}]}
elif prompt in {"inspect", "inspect-denied"} and messages[-1]["role"] != "tool":
    output = {"content": "", "tool_calls": [{"id": "inspect-1", "name": "run_command", "arguments": {"program": "inspect_os" if prompt == "inspect" else "unauthorized_shell"}}]}
else:
    output = {"content": messages[-1]["content"] if messages[-1]["role"] == "tool" else ",".join(names), "tool_calls": []}
print(json.dumps({"type": "result", "subtype": "success", "structured_output": output}))
