"""Check the native CLI versions and their configuration formats in disposable Linux paths."""
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
with tempfile.TemporaryDirectory(prefix="agenthub-live-") as directory:
    root = pathlib.Path(directory)
    home, project = root / "home", root / "project"
    shutil.copytree(fixtures / "home", home)
    shutil.copytree(fixtures / "project", project)
    env = dict(os.environ, HOME=str(home), CODEX_HOME=str(home / ".codex"), CLAUDE_CONFIG_DIR=str(home / ".claude"),
               XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_CACHE_HOME=str(root / "cache"), XDG_STATE_HOME=str(root / "state"),
               OPENCODE_DISABLE_AUTOUPDATE="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
    def run(args, cwd=project):
        return subprocess.check_output(args, cwd=cwd, env=env, timeout=30, stderr=subprocess.PIPE)

    for program, version in [("codex", "0.167.0-alpha.4"), ("opencode", "1.19.6"), ("claude", "2.1.327")]:
        assert version in run([program, "--version"]).decode().split()
    entry = json.loads(run(["codex", "mcp", "get", "--json", "--", "handbook"]))
    assert entry["enabled"] and entry["transport"]["command"] in ("fixture-global", "fixture-project")
    print("PASS native CLI versions and Codex MCP lookup")
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
