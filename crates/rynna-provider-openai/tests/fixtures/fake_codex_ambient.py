#!/usr/bin/env python3
"""Reproduce inherited MCP authority without starting a real MCP server."""
import json
import sys
from pathlib import Path

if sys.argv[1:] == ["mcp", "list", "--json"]:
    scenario = Path(sys.argv[0]).name
    if scenario == "inventory-failed":
        print("private-config-canary", file=sys.stderr)
        sys.exit(1)
    if scenario == "inventory-malformed":
        print("private-config-canary")
        sys.exit(0)
    if scenario == "inventory-oversized":
        print("x" * (1024 * 1024 + 1))
        sys.exit(0)
    if scenario == "inventory-missing-name":
        print('[{}]')
        sys.exit(0)
    if scenario == "inventory-unsupported":
        print('[{"name":"private-config-canary", "transport":{"type":"unknown"}}]')
        sys.exit(0)
    servers = [{"name": name, "enabled": True, "transport": {"type": "stdio"}} for name in ["cua_repl", "server.with.dots", "already-disabled"]]
    servers.append({"name": "remote", "enabled": True, "transport": {"type": "streamable_http"}})
    print(json.dumps(servers))
    sys.exit(0)


def send(value):
    print(json.dumps(value), flush=True)


config = {}
for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if method == "initialize":
        send({"id": request["id"], "result": {}})
    elif method == "config/read":
        send({"id": request["id"], "result": {"config": {"mcp_servers": {
            "server.with.dots": {"enabled": True},
            "already-disabled": {"enabled": False}
        }}}})
    elif method == "thread/start":
        config = request["params"]["config"]
        send({"id": request["id"], "result": {"thread": {"id": "t"}}})
    elif method == "turn/start":
        send({"id": request["id"], "result": {"turn": {"id": "u"}}})
        expected = {name: {"enabled": False, "command": "rynna-disabled-mcp"} for name in ["cua_repl", "server.with.dots", "already-disabled"]}
        expected["remote"] = {"enabled": False, "url": "http://127.0.0.1:1"}
        if config.get("mcp_servers") != expected:
            send({"method": "item/started", "params": {"threadId": "t", "turnId": "u", "item": {"id": "ambient", "type": "mcpToolCall", "server": "cua_repl", "tool": "js"}}})
        else:
            send({"method": "item/started", "params": {"threadId": "t", "turnId": "u", "item": {"type": "agentMessage", "id": "a"}}})
            send({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "u", "itemId": "a", "delta": "No ambient tools available"}})
            send({"method": "turn/completed", "params": {"threadId": "t", "turn": {"id": "u", "status": "completed"}}})
        break
