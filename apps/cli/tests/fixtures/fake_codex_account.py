#!/usr/bin/env python3
"""Reviewed-protocol double; never contacts OpenAI or reads real credentials."""
import json
import os
import sys
import time
from pathlib import Path

if sys.argv[1:] == ["--version"]:
    print("codex-cli 0.149.1")
    sys.exit(0)
if sys.argv[1:] == ["login", "status"]:
    print("Logged in using ChatGPT", file=sys.stderr)
    sys.exit(0)
if sys.argv[1:] == ["login"]:
    assert os.environ["CODEX_HOME"].endswith("rynna-codex")
    root = Path(os.environ["CODEX_HOME"]).parent
    (root / "login-started").touch()
    deadline = time.monotonic() + 5
    while (root / "delay-login").exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    sys.exit(0)
assert sys.argv[1:] == ["app-server"]


def emit(value):
    print(json.dumps(value), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    method = message["method"]
    if method == "initialized":
        continue
    result = {}
    if method == "model/list":
        assert message["params"]["includeHidden"] is False
        if message["params"].get("cursor") is None:
            result = {"data": [{"id": "picker-id", "model": "account-model"}], "nextCursor": "next"}
        else:
            assert message["params"]["cursor"] == "next"
            result = {"data": [{"model": "second-model"}, {"model": "hidden", "hidden": True}], "nextCursor": None}
    elif method == "thread/start":
        params = message["params"]
        assert params["environments"] == []
        assert params["ephemeral"] is True
        assert params["sandbox"] == "read-only"
        assert params["config"]["features"]["shell_tool"] is False
        assert params["config"]["tools"]["update_plan"]["enabled"] is False
        assert params["config"]["web_search"] == "disabled"
        with open(os.environ["RYNNA_TEST_LOG"], "a") as output:
            output.write(json.dumps({"model": params.get("model"), "home": os.environ.get("CODEX_HOME")}) + "\n")
        result = {"thread": {"id": "thread-1"}}
    elif method == "turn/start":
        result = {"turn": {"id": "turn-1"}}
    emit({"id": message["id"], "result": result})
    if method == "turn/start":
        context = {"threadId": "thread-1", "turnId": "turn-1"}
        emit({"method": "item/started", "params": dict(context, item={"type": "agentMessage", "id": "answer"})})
        emit({"method": "item/reasoning/summaryTextDelta", "params": dict(context, delta="Checking.")})
        emit({"method": "item/agentMessage/delta", "params": dict(context, itemId="answer", delta="Account answer.")})
        emit({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}}})
