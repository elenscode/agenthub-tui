"""Run the interactive wizard, search, watcher and guarded edit in a private POSIX PTY."""
import pyte
import fcntl
import os
import pathlib
import pty
import select
import shutil
import signal
import struct
import sys
import tempfile
import termios
import time

binary = pathlib.Path(sys.argv[1]).resolve()
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
with tempfile.TemporaryDirectory(prefix="agenthub-tui-") as temporary:
    root = pathlib.Path(temporary)
    for subdir in ("home", "project"):
        shutil.copytree(fixtures / subdir, root / subdir)
    project = root / "project"
    env = os.environ.copy()
    env.update({
        "TERM": "xterm-256color",
        "AGENTHUB_STATE_DIR": str(root / "state"),
        "CODEX_HOME": str(root / "home/.codex"),
        "CLAUDE_CONFIG_DIR": str(root / "home/.claude"),
        "XDG_CONFIG_HOME": str(root / "home/.config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "xdgstate"),
        "OPENCODE_DISABLE_AUTOUPDATE": "1",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    })
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(1, termios.TIOCSWINSZ, struct.pack("HHHH", 42, 140, 0, 0))
        os.chdir(project)
        os.execve(binary, [str(binary), "--setup", "--fixture-home", str(root / "home")], env)
    transcript = bytearray()
    screen = pyte.Screen(140, 42)
    stream = pyte.ByteStream(screen)
    cursor = 0

    def display():
        return "\n".join("".join(screen.buffer[y][x].data for x in range(screen.columns)) for y in range(screen.lines))

    def expect(phrase, seconds=16):
        global cursor
        needle = phrase.encode()
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            at = transcript.find(needle, cursor)
            if at >= 0 or phrase in display():
                cursor = len(transcript) if at < 0 else at + len(needle)
                return
            if select.select([fd], [], [], 0.15)[0]:
                data = os.read(fd, 131072)
                if not data:
                    break
                transcript.extend(data)
                stream.feed(data)
        raise AssertionError("Terminal did not display " + phrase + "\n" + display())

    def send(value):
        os.write(fd, value.encode())

    finished = False
    try:
        expect("프로젝트 [")
        send("\r")
        expect("상태 메시지")
        send("/kumquat\r")
        expect("kumquat")
        skill = project / ".agents/skills/review/SKILL.md"
        skill.write_text(skill.read_text().replace("kumquat", "watch-verified"))
        expect("검색 결과 없음", 12)
        send("/" + "\x7f" * 7 + "manual-http\r")
        expect("manual-http")
        send(":delete\r")
        expect("변경 미리보기")
        mcp = project / ".mcp.json"
        before = mcp.read_bytes()
        send("\x1b")
        expect("자산 탐색")
        assert mcp.read_bytes() == before
        send(":delete\r")
        expect("변경 미리보기")
        send("a")
        expect("DPAPI")
        assert mcp.read_bytes() == before
        send("\x1b")
        time.sleep(0.2)
        send(":export\r")
        expect("진단 요약 내보내기")
        send("y")
        expect("내보내기 완료")
        export = next((root / "state").glob("diagnostics-*.json")).read_bytes()
        assert b"CANARY-ONLY-FOR-TESTS" not in export
        assert b"CANARY-ONLY-FOR-TESTS" not in transcript
        assert b"CANARY-ONLY-FOR-TESTS" not in (root / "state/audit.jsonl").read_bytes()
        send("\x1b")
        time.sleep(0.2)
        send("q")
        for _ in range(60):
            got, status = os.waitpid(pid, os.WNOHANG)
            if got:
                assert os.waitstatus_to_exitcode(status) == 0
                finished = True
                break
            time.sleep(0.05)
        assert finished
        print("PASS wizard, live search, watcher refresh, delete diff, cancel, encryption failure, approved export, redaction, clean exit")
    finally:
        if not finished:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(fd)

