"""Isolated stdio fixture. Only records method names, never accesses credentials."""
import json
import pathlib
import sys
import time

mode, log = sys.argv[1:]
for line in sys.stdin:
    message = json.loads(line)
    method = message["method"]
    with pathlib.Path(log).open("a", encoding="utf-8") as stream:
        stream.write(method + "\n")
    if mode == "timeout":
        time.sleep(10)
    if mode == "noisy":
        sys.stdout.write("x" * 140000 + "\n")
        sys.stdout.flush()
        continue
    if mode == "failure":
        sys.exit(2)
    if method == "initialize":
        result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}}
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        result = {"tools": [{"name": "read_fixture", "description": "CANARY-ONLY-FOR-TESTS"}]}
    else:
        sys.exit(3)
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)

