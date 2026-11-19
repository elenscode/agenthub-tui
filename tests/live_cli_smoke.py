"""Check the pinned CLIs and the one-shot Linux Helper with disposable paths."""
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import uuid

host, helper = [str(pathlib.Path(a).resolve()) for a in sys.argv[1:]]
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
with tempfile.TemporaryDirectory(prefix="agenthub-live-") as directory:
    root = pathlib.Path(directory)
    home, project = root / "home", root / "project"
    shutil.copytree(fixtures / "home", home)
    shutil.copytree(fixtures / "project", project)
    env = dict(os.environ, CODEX_HOME=str(home / ".codex"), CLAUDE_CONFIG_DIR=str(home / ".claude"),
               XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_CACHE_HOME=str(root / "cache"), XDG_STATE_HOME=str(root / "state"),
               OPENCODE_DISABLE_AUTOUPDATE="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
    def run(args, cwd=project):
        return subprocess.check_output(args, cwd=cwd, env=env, timeout=30, stderr=subprocess.PIPE)

    registry = json.loads(run([host, "--scan", "--project", str(project), "--fixture-home", str(home)]))
    versions = {"Codex": "0.167.0-alpha.4", "OpenCode": "1.19.6", "Claude": "2.1.327"}
    assert registry["versions"] == versions
    assert len({a["kind"] for a in registry["assets"]}) == 7
    recognized = [a for a in registry["assets"] if a["provider"] == "Codex" and a["name"] == "handbook" and a["confidence"] == "Verified"]
    assert len(recognized) == 1 and recognized[0]["effective"]
    print("PASS installed versions, seven asset kinds, verified Codex MCP source")
    plain = root / "isolated-project"
    plain.mkdir()
    entry = {"type": "local", "command": ["fixture-only"], "enabled": False}
    (plain / "opencode.json").write_text(json.dumps({"mcp": {"acceptance": entry}}))
    resolved = json.loads(run(["opencode", "debug", "config", "--pure"], plain))
    assert resolved["mcp"]["acceptance"] == entry
    run(["claude", "mcp", "add-json", "--scope", "project", "acceptance",
         '{"type":"stdio","command":"fixture-only"}'], plain)
    assert json.loads((plain / ".mcp.json").read_text())["mcpServers"]["acceptance"]["command"] == "fixture-only"
    print("PASS OpenCode resolved disabled entry and Claude wrote isolated project schema")
    context = {"environment": "WSL/fixture", "project": str(project), "home": str(home), "codex": str(home / ".codex"), "claude": str(home / ".claude"), "opencode": str(home / ".config/opencode"), "policy": str(home / "policy")}
    for leased, operation in [(False, "Hello"), (True, "Hello"), (True, "Scan")]:
        req = {"protocol": 1, "id": str(uuid.uuid4()), "context": context, "action": operation}
        with subprocess.Popen([helper] + (["--leased"] if leased else []), stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env) as child:
            child.stdin.write((json.dumps(req) + "\n").encode())
            child.stdin.flush()
            if not leased:
                child.stdin.close()
            # Reading the complete reply before closing stdin tests the host's lease contract.
            import select
            assert select.select([child.stdout], [], [], 20)[0], "Helper produced no reply"
            reply = json.load(child.stdout)
            assert child.wait(timeout=5) == 0
            assert reply["id"] == req["id"] and reply["protocol"] == 1 and reply["error"] is None
            if operation == "Scan":
                assert any(a["name"] == "handbook" for a in reply["data"]["assets"])
    print("PASS one-shot and leased Helper round trips, scan, request correlation and process exit")
