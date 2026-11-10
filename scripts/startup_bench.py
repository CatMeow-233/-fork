#!/usr/bin/env python3
"""Measure how long `codex` takes to start its first turn.

The benchmark runs the TUI under a pseudo-terminal that answers the usual
terminal probes, types a prompt as soon as the composer is drawn, and records
when a local mock of the Responses API receives the first `/responses`
request. It needs only the Python standard library.

Examples:
    # Embedded app server, fresh CODEX_HOME for every run.
    scripts/startup_bench.py --codex codex-rs/target/debug/codex --no-daemon

    # Background daemon: first run installs it, later runs reuse it.
    scripts/startup_bench.py --codex codex-rs/target/debug/codex --daemon --runs 5

    # Daemon is installed but stopped before every run.
    scripts/startup_bench.py --codex codex-rs/target/debug/codex --daemon --stop-daemon

    # Simulate a metrics collector that is 400 ms away.
    scripts/startup_bench.py --codex codex-rs/target/debug/codex --daemon --metrics-rtt-ms 400

    # CPU cost of rendering a long Markdown reply (compare two builds).
    scripts/startup_bench.py --codex codex-rs/target/release/codex --no-daemon --reply-kb 200

Daemon mode needs a packaged CLI, so the binary is copied into a minimal
package layout under the benchmark's temporary directory.
"""

from __future__ import annotations

import argparse
import fcntl
import http.server
import json
import os
import pty
import re
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from pathlib import Path

PROMPT_MARKER = "Ask Codex"
REPLY_TEXT = "MOCK-REPLY"
ANSI_RE = re.compile(
    rb"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Z0-9]|\x1b[=>78]"
)
# Replies a typical terminal sends to the startup probes.
PROBE_REPLIES = [
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b[6n", b"\x1b[1;1R"),
    (b"\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
    (b"\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
    (b"\x1b[>c", b"\x1b[>41;350;0c"),
    (b"\x1b[c", b"\x1b[?62;22c"),
]


class MockResponses(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, metrics_delay: float, reply_text: str) -> None:
        super().__init__(("127.0.0.1", 0), _Handler)
        self.metrics_delay = metrics_delay
        self.reply_text = reply_text
        self.lock = threading.Lock()
        self.first_responses_post: float | None = None

    def reset(self) -> None:
        with self.lock:
            self.first_responses_post = None


class _Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server: MockResponses

    def log_message(self, *_args: object) -> None:
        pass

    def _reply(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        if self.path.startswith("/v1/models"):
            self._reply(
                200, b'{"object":"list","data":[],"models":[]}', "application/json"
            )
        else:
            self._reply(404, b"{}", "application/json")

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length") or 0)
        if length:
            self.rfile.read(length)
        if "/v1/metrics" in self.path:
            time.sleep(self.server.metrics_delay)
            self._reply(200, b"{}", "application/json")
            return
        if "/responses" not in self.path:
            self._reply(404, b"{}", "application/json")
            return
        with self.server.lock:
            if self.server.first_responses_post is None:
                self.server.first_responses_post = time.monotonic()
        events = [
            {"type": "response.created", "response": {"id": "resp_1"}},
            {
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "id": "msg_1",
                    "content": [
                        {"type": "output_text", "text": self.server.reply_text}
                    ],
                },
            },
            {
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
                },
            },
        ]
        body = "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events)
        self._reply(200, body.encode(), "text/event-stream")


def reply_text(kilobytes: int) -> str:
    """A Markdown reply of about `kilobytes` KB that ends with the marker."""
    section = (
        "## Section {n}\n\n"
        "Plain prose with **bold**, _emphasis_, `inline code` and a "
        "[link](https://example.com/{n}) so wrapping and styling both run.\n\n"
        "- first item {n}\n- second item with `code`\n  - nested item\n\n"
        "```rust\nfn section_{n}(input: &str) -> Option<usize> {{\n"
        "    let total: usize = input.lines().map(str::len).sum();\n"
        '    (total > {n}).then_some(total) // "string" and 0x{n:x}\n}}\n```\n\n'
        "```json\n"
        '{{"id": {n}, "name": "section-{n}", "tags": ["a", "b"], "ok": true}}\n'
        "```\n\n"
        "| column | value |\n|---|---|\n| n | {n} |\n| square | {sq} |\n\n"
    )
    parts: list[str] = []
    size = 0
    n = 0
    while size < kilobytes * 1024:
        n += 1
        parts.append(section.format(n=n, sq=n * n))
        size += len(parts[-1])
    parts.append(REPLY_TEXT)
    return "".join(parts)


def write_config(home: Path, workdir: Path, port: int, metrics: bool) -> None:
    home.mkdir(parents=True, exist_ok=True)
    (home / "auth.json").write_text('{"OPENAI_API_KEY":"sk-startup-bench"}')
    lines = [
        'model_provider = "bench"',
        'model = "gpt-5.1-codex"',
        "[model_providers.bench]",
        'name = "bench"',
        f'base_url = "http://127.0.0.1:{port}/v1"',
        'wire_api = "responses"',
    ]
    if metrics:
        lines += [
            "[otel]",
            (
                "metrics_exporter = { otlp-http = { endpoint = "
                f'"http://127.0.0.1:{port}/v1/metrics", protocol = "json" }} }}'
            ),
        ]
    lines += [f'[projects."{workdir}"]', 'trust_level = "trusted"']
    (home / "config.toml").write_text("\n".join(lines) + "\n")


def package_binary(codex: Path, root: Path) -> Path:
    """Copy `codex` into the minimal package layout the daemon installer accepts."""
    for sub in ("bin", "codex-path", "codex-resources"):
        (root / sub).mkdir(parents=True, exist_ok=True)
    entry = root / "bin" / "codex"
    shutil.copy2(codex, entry)
    stub = shutil.which("true") or "/bin/true"
    rg = shutil.which("rg") or stub
    for dest, src in (
        (root / "bin" / "codex-code-mode-host", stub),
        (root / "codex-path" / "rg", rg),
        (root / "codex-resources" / "bwrap", stub),
    ):
        shutil.copy2(src, dest)
    machine = os.uname().machine
    arch = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
    target = (
        f"{arch}-apple-darwin"
        if sys.platform == "darwin"
        else f"{arch}-unknown-linux-gnu"
    )
    manifest = {
        "version": "0.0.0",
        "target": target,
        "entrypoint": "bin/codex",
        "layoutVersion": 2,
    }
    (root / "codex-package.json").write_text(json.dumps(manifest))
    return entry


def stop_daemon(home: Path) -> None:
    subprocess.run(["pkill", "-f", str(home / "packages")], check=False)
    time.sleep(0.7)


def run_once(
    codex: Path,
    home: Path,
    workdir: Path,
    server: MockResponses,
    args: argparse.Namespace,
) -> dict:
    env = dict(os.environ, CODEX_HOME=str(home), TERM="xterm-256color")
    env["NO_PROXY"] = ",".join(
        filter(None, ["127.0.0.1", "localhost", env.get("NO_PROXY")])
    )
    env["no_proxy"] = env["NO_PROXY"]
    argv = [str(codex)] + (["--no-daemon"] if not args.daemon else [])
    server.reset()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        os.execve(argv[0], argv, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    start = time.monotonic()
    screen = ""
    draft_at = reply_at = None
    submitted = False
    try:
        while time.monotonic() - start < args.timeout:
            ready, _, _ = select.select([fd], [], [], 0.01)
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            for query, reply in PROBE_REPLIES:
                if query in data:
                    os.write(fd, reply)
            screen += ANSI_RE.sub(b"", data).decode("utf-8", "replace")
            if draft_at is None and PROMPT_MARKER in screen:
                draft_at = time.monotonic() - start
            if draft_at is not None and not submitted:
                os.write(fd, b"hello")
                # Typed faster than this, Enter counts as part of a paste.
                time.sleep(args.enter_delay)
                os.write(fd, b"\r")
                submitted = True
            if REPLY_TEXT in screen:
                reply_at = time.monotonic() - start
                break
    finally:
        try:
            os.kill(pid, signal.SIGTERM)
            time.sleep(0.3)
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        _, _, usage = os.wait4(pid, 0)
        os.close(fd)
    first_post = server.first_responses_post
    return {
        "composer_visible_s": draft_at,
        "first_request_s": None if first_post is None else first_post - start,
        "reply_rendered_s": reply_at,
        # CPU the TUI process itself used (a daemon's share is not included).
        "cpu_s": usage.ru_utime + usage.ru_stime,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--codex", type=Path, required=True, help="path to the codex binary"
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--daemon", action="store_true", help="use the background app-server daemon"
    )
    mode.add_argument(
        "--no-daemon", action="store_true", help="use the embedded app server (default)"
    )
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument(
        "--stop-daemon", action="store_true", help="stop the daemon before every run"
    )
    parser.add_argument(
        "--fresh-home", action="store_true", help="use a new CODEX_HOME for every run"
    )
    parser.add_argument(
        "--metrics-rtt-ms",
        type=float,
        default=None,
        help="configure a slow OTLP metrics exporter",
    )
    parser.add_argument(
        "--reply-kb",
        type=int,
        default=0,
        help="make the mock reply a Markdown document of about this many KB",
    )
    parser.add_argument("--enter-delay", type=float, default=0.3)
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()

    server = MockResponses(
        metrics_delay=(args.metrics_rtt_ms or 0) / 1000,
        reply_text=reply_text(args.reply_kb) if args.reply_kb else REPLY_TEXT,
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    port = server.server_address[1]
    results = []
    with tempfile.TemporaryDirectory(prefix="codex-startup-bench-") as tmp:
        tmp_path = Path(tmp)
        workdir = tmp_path / "work"
        workdir.mkdir()
        subprocess.run(["git", "init", "-q", str(workdir)], check=True)
        codex = (
            package_binary(args.codex.resolve(), tmp_path / "package")
            if args.daemon
            else args.codex.resolve()
        )
        home = tmp_path / "home"
        for run in range(args.runs):
            if args.fresh_home or run == 0:
                if args.daemon and home.exists():
                    stop_daemon(home)
                shutil.rmtree(home, ignore_errors=True)
                write_config(home, workdir, port, args.metrics_rtt_ms is not None)
            elif args.daemon and args.stop_daemon:
                stop_daemon(home)
            result = run_once(codex, home, workdir, server, args)
            result["run"] = run + 1
            results.append(result)
            print(json.dumps(result), flush=True)
        if args.daemon:
            stop_daemon(home)
    server.shutdown()
    samples = [
        r["first_request_s"] for r in results if r["first_request_s"] is not None
    ]
    if samples:
        print(
            f"first request: median {statistics.median(samples):.3f}s, min {min(samples):.3f}s, max {max(samples):.3f}s"
        )
    cpu = [r["cpu_s"] for r in results]
    print(f"process CPU: median {statistics.median(cpu):.3f}s, min {min(cpu):.3f}s")
    return 0 if len(samples) == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
