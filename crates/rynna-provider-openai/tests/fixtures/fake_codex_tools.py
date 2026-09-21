#!/usr/bin/env python3
"""Deterministic app-server protocol peer; never executes a requested tool."""
import json
import sys

if sys.argv[1:] == ["mcp", "list", "--json"]:
    print("[]")
    sys.exit(0)


def read():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value), flush=True)


def reply(request, result):
    send({"id": request["id"], "result": result})


reply(read(), {})
assert read()["method"] == "initialized"
thread = read()
p = thread["params"]
assert p["approvalPolicy"] == "never" and p["sandbox"] == "read-only"
assert p["config"]["features"]["shell_tool"] is False
assert p["config"]["features"]["view_image"] is False
assert p["config"]["web_search"] == "disabled"
assert p["dynamicTools"] == [] or p["dynamicTools"] == [{"type": "function", "name": "inspect_host", "description": "Inspect", "inputSchema": {"type": "object"}}]
reply(thread, {"thread": {"id": "t"}})
turn = read()
if p["baseInstructions"].startswith("Replay test: "):
    import os
    from pathlib import Path

    identity = p["baseInstructions"].splitlines()[0].removeprefix("Replay test: ")
    assert identity in ("solo", "alpha", "beta")
    items = []
    if turn["method"] == "thread/inject_items":
        items = turn["params"]["items"]
        reply(turn, {})
        turn = read()
    round_number = sum(item["type"] == "function_call_output" for item in items)
    home = Path(os.environ["CODEX_HOME"])
    (home / f"{identity}-{round_number}.json").write_text(json.dumps({
        "items": items, "turn": turn,
    }))
    if identity != "solo":
        import time

        # Ready markers are published only after the entire wire capture closes.
        # Both live processes must reach each round before either emits a result.
        (home / f"{identity}-{round_number}.ready").touch()
        peer = "beta" if identity == "alpha" else "alpha"
        deadline = time.monotonic() + 10
        while not (home / f"{peer}-{round_number}.ready").exists():
            assert time.monotonic() < deadline, "concurrent peer never reached this round"
            time.sleep(0.01)
    reply(turn, {"turn": {"id": "u"}})
    if round_number < 3:
        send({"id": "rpc-1", "method": "item/tool/call", "params": {
            "threadId": "t", "turnId": "u", "callId": f"call-{round_number + 1}",
            "tool": "inspect_host", "arguments": {
                "conversation": identity, "step": round_number + 1,
            },
        }})
        sys.stdin.read()
    else:
        send({"method": "item/started", "params": {"threadId": "t", "turnId": "u", "item": {"type": "agentMessage", "id": "a"}}})
        send({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "u", "itemId": "a", "delta": f"Verified {identity}"}})
        send({"method": "turn/completed", "params": {"threadId": "t", "turn": {"id": "u", "status": "completed"}}})
    sys.exit(0)
if turn["method"] == "thread/inject_items":
    items = turn["params"]["items"]
    assert items[-2] == {"type": "function_call", "call_id": "call-1", "name": "inspect_host", "arguments": '{"kind":"os"}'}
    assert items[-1]["type"] == "function_call_output" and items[-1]["call_id"] == "call-1"
    output = items[-1]["output"]
    assert output in ("Authorized host result", '"Authorized host result"', '{"error":"tool failed: permission denied"}')
    answer = "Permission denied" if "permission denied" in output else "Host verified"
    reply(turn, {})
    turn = read()
    reply(turn, {"turn": {"id": "u"}})
    if p["baseInstructions"].startswith("Reused call test"):
        send({"id": "rpc-2", "method": "item/tool/call", "params": {
            "threadId": "t", "turnId": "u", "callId": "call-1",
            "tool": "inspect_host", "arguments": {"kind": "os"},
        }})
        sys.stdin.read()
        sys.exit(0)
    send({"method": "item/started", "params": {"threadId": "t", "turnId": "u", "item": {"type": "agentMessage", "id": "a"}}})
    send({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "u", "itemId": "a", "delta": answer}})
    send({"method": "turn/completed", "params": {"threadId": "t", "turn": {"id": "u", "status": "completed"}}})
else:
    reply(turn, {"turn": {"id": "u"}})
    send({"method": "item/started", "params": {"threadId": "t", "turnId": "u", "item": {"type": "dynamicToolCall", "id": "call-1", "tool": "inspect_host", "arguments": {"kind": "os"}}}})
    request = {"id": "rpc-1", "method": "item/tool/call", "params": {"threadId": "t", "turnId": "u", "callId": "call-1", "tool": "inspect_host", "arguments": {"kind": "os"}}}
    scenario = turn["params"]["input"][0]["text"]
    if scenario == "missing-id":
        del request["id"]
    elif scenario == "unsupported":
        request["method"] = "item/commandExecution/requestApproval"
    elif scenario == "unadvertised":
        request["params"]["tool"] = "shell"
    elif scenario == "wrong-turn":
        request["params"]["turnId"] = "other"
    elif scenario == "malformed-arguments":
        request["params"]["arguments"] = "not an object"
    elif scenario == "namespace":
        request["params"]["namespace"] = "ambient"
    send(request)
    if scenario != "Inspect host":
        sys.exit(0)
    sys.stdin.read()
