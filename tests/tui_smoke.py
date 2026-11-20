"""Drive the Linux release in a real terminal with disposable projects and state."""
import fcntl
import json
import os
import pathlib
import pty
import select
import shutil
import signal
import stat
import struct
import sys
import tempfile
import termios
import time

import pyte

binary = pathlib.Path(sys.argv[1]).resolve()
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
with tempfile.TemporaryDirectory(prefix="agenthub-tui-") as temporary:
    root = pathlib.Path(temporary)
    home, project, state = root / "home", root / "project", root / "state"
    shutil.copytree(fixtures / "home", home)
    shutil.copytree(fixtures / "project", project)
    nested = project / "zz-work" / "nested"
    nested.mkdir(parents=True)
    config = nested / ".mcp.json"
    config.write_text('{"mcpServers":{"nested-server":{"command":"fixture-only"}}}')
    env = dict(os.environ, TERM="xterm-256color", HOME=str(home), AGENTHUB_STATE_DIR=str(state),
               CODEX_HOME=str(home / ".codex"), CLAUDE_CONFIG_DIR=str(home / ".claude"),
               XDG_CONFIG_HOME=str(home / ".config"), XDG_DATA_HOME=str(root / "data"),
               XDG_CACHE_HOME=str(root / "cache"), XDG_STATE_HOME=str(root / "xdgstate"),
               OPENCODE_DISABLE_AUTOUPDATE="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")

    def set_size(fd, width):
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 42, width, 0, 0))

    pid, fd = pty.fork()
    if pid == 0:
        set_size(1, 140)
        os.chdir(root)
        os.execve(binary, [str(binary), str(project)], env)

    transcript = bytearray()
    screen = pyte.Screen(140, 42)
    stream = pyte.ByteStream(screen)

    def display():
        return "\n".join("".join(screen.buffer[y][x].data for x in range(screen.columns))
                         for y in range(screen.lines))

    def expect(phrase, absent=None, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            text = display()
            if phrase in text and (absent is None or absent not in text):
                return
            if select.select([fd], [], [], 0.1)[0]:
                data = os.read(fd, 131072)
                transcript.extend(data)
                stream.feed(data)
        raise AssertionError(f"Expected {phrase!r}, absent {absent!r}\n{display()}")

    def send(keys):
        os.write(fd, keys.encode())

    def resize(width):
        screen.resize(columns=width)
        set_size(fd, width)
        os.kill(pid, signal.SIGWINCH)

    finished = False
    try:
        expect("상태 메시지")
        expect("폴더 · f")
        send("/kumquat\r")
        expect("Skill  review")
        skill = project / ".agents/skills/review/SKILL.md"
        skill.write_text(skill.read_text().replace("kumquat", "watch-verified"))
        expect("검색 결과 없음", seconds=12)
        send("/" + "\x7f" * 7 + "manual-http\r")
        expect("Mcp  manual-http")
        send(":delete\r")
        expect("변경 미리보기")
        mcp = project / ".mcp.json"
        before = mcp.read_bytes()
        send("\x1b")
        expect("자산 탐색", absent="변경 미리보기")
        assert mcp.read_bytes() == before and not (state / "keys").exists()
        send(":delete\r")
        expect("변경 미리보기")
        send("a")
        expect("적용 및 재검증 완료")
        assert "manual-http" not in json.loads(mcp.read_text())["mcpServers"]
        backup, = (state / "backups").glob("*.ahb")
        assert b"CANARY-ONLY-FOR-TESTS" not in backup.read_bytes()
        assert stat.S_IMODE((state / "keys/backup.key").stat().st_mode) == 0o600
        send("\x1b")
        expect("자산 탐색")
        send(":restore\r")
        expect("백업 ID")
        send(backup.stem + "\r")
        expect("변경 미리보기")
        send("a")
        expect("작업 결과")
        assert before == mcp.read_bytes()
        send("\x1b")
        expect("자산 탐색")
        send(":export\r")
        expect("진단 요약 내보내기")
        send("y")
        expect("내보내기 완료")
        assert b"CANARY-ONLY-FOR-TESTS" not in next(state.glob("diagnostics-*.json")).read_bytes()
        assert b"CANARY-ONLY-FOR-TESTS" not in (state / "audit.jsonl").read_bytes()
        send("\x1b")
        expect("자산 탐색")

        resize(70)
        expect("자산 탐색", absent="폴더 · f")
        send("f")
        expect("폴더 · f", absent="자산 탐색")
        send("j" * 12 + "ll")
        expect("›     ▸ nested")
        send("\r")
        expect("Mcp  nested-server", absent="manual-http")
        config.write_text(config.read_text().replace("nested-server", "watch-new-project"))
        expect("Mcp  watch-new-project", absent="nested-server", seconds=12)
        resize(140)
        expect("폴더 · f")
        send("f\x7f\x7f\r")
        expect("Mcp  manual-http", absent="watch-new-project")
        assert b"CANARY-ONLY-FOR-TESTS" not in transcript
        send("q")
        for _ in range(80):
            got, status = os.waitpid(pid, os.WNOHANG)
            if got:
                finished = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            time.sleep(0.05)
        assert finished
        print("PASS search, cancel, approved edit and restore, export, redaction, folder navigation, project watches and resize")
    finally:
        if not finished:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(fd)
