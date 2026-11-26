#!/usr/bin/env python3
"""Record an omp `--mode rpc-ui` conversation as a fixture.

Usage: record.py SCENARIO [--omp PATH] [--out FILE]

Every frame is written verbatim, one per line, prefixed by its direction: `< ` for a frame omp wrote, `> ` for a
command or UI response sent to it. The home directory is replaced by `/Users/USER`, and the scratch directory by
`/tmp/scratch`. Each scenario runs omp in a fresh temporary directory with its own `--session-dir`, never in a
real repository, and never logs in or approves anything on the user's behalf beyond what the scenario states.
"""
import argparse
import base64
import http.server
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

HOME = os.path.expanduser("~")
REAL_HOME = os.path.realpath(HOME)
TMPDIR = tempfile.gettempdir().rstrip("/")
REAL_TMPDIR = os.path.realpath(TMPDIR).rstrip("/")


class Session:
    def __init__(self, omp, args, cwd, log, env=None):
        self.log = log
        self.q = queue.Queue()
        proc_env = None
        if env:
            proc_env = os.environ.copy()
            proc_env.update(env)
        self.p = subprocess.Popen(
            [omp, "--mode", "rpc-ui", "--no-title", *args],
            cwd=cwd,
            env=proc_env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        chunks = {}
        for line in self.p.stdout:
            line = line.rstrip("\n")
            self.log("< " + line)
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            if isinstance(obj, dict) and obj.get("type") == "rpc_chunk":
                cid = obj.get("chunkId")
                idx = obj.get("index")
                cnt = obj.get("count")
                data = obj.get("data")
                if isinstance(cid, str) and isinstance(idx, int) and isinstance(cnt, int) and isinstance(data, str):
                    entry = chunks.setdefault(cid, {"count": cnt, "parts": {}})
                    entry["parts"][idx] = base64.b64decode(data)
                    if len(entry["parts"]) == entry["count"]:
                        raw = b"".join(entry["parts"][i] for i in range(entry["count"]))
                        chunks.pop(cid, None)
                        try:
                            obj = json.loads(raw.decode("utf-8"))
                        except ValueError:
                            continue
                    else:
                        continue
            self.q.put(obj)
        self.q.put(None)

    def send(self, obj):
        line = json.dumps(obj, separators=(",", ":"))
        self.log("> " + line)
        self.p.stdin.write(line + "\n")
        self.p.stdin.flush()

    def wait(self, pred, timeout=90, on_frame=None):
        end = time.time() + timeout
        while time.time() < end:
            try:
                f = self.q.get(timeout=0.5)
            except queue.Empty:
                continue
            if f is None:
                return None
            if on_frame:
                on_frame(f)
            if pred(f):
                return f
        return None

    def close(self):
        try:
            self.p.stdin.close()
        except Exception:
            pass
        try:
            self.p.wait(timeout=20)
        except Exception:
            self.p.kill()


def is_resp(id_):
    return lambda f: f.get("type") == "response" and f.get("id") == id_


def settled(f):
    return f.get("type") == "session_settled"


def prompt_done(id_):
    return lambda f: f.get("type") == "prompt_result" and f.get("id") == id_


def negotiate(s):
    s.send({"id": "neg", "type": "negotiate_protocol", "protocolVersion": 2})
    s.wait(is_resp("neg"), 10)
    s.send({"id": "ev", "type": "set_event_filter", "events": None, "messageUpdates": "delta"})
    s.wait(is_resp("ev"), 10)


def scenario_plain(s, cwd, **_):
    open(os.path.join(cwd, "a.txt"), "w").write("hello from a.txt\nsecond line\n")
    negotiate(s)
    s.send({"id": "p1", "type": "prompt", "message": "Read a.txt with the read tool and reply with only its first line."})
    s.wait(prompt_done("p1"))
    s.send({"id": "st", "type": "get_state"})
    s.wait(is_resp("st"), 10)
    s.send({"id": "stats", "type": "get_session_stats"})
    s.wait(is_resp("stats"), 10)


def scenario_ask(s, cwd, **_):
    negotiate(s)
    s.send({"id": "p1", "type": "prompt", "message": "Use the ask tool to ask me one question: which colour do I prefer, with options red and blue. Then reply with only my answer."})

    def answer(f):
        if f.get("type") == "extension_ui_request" and f.get("method") not in ("setWidget", "setStatus", "notify", "setTitle", "set_editor_text"):
            if f["method"] == "form":
                s.send({"type": "extension_ui_response", "id": f["id"], "data": {"colour": "blue"}})
            elif f["method"] == "confirm":
                s.send({"type": "extension_ui_response", "id": f["id"], "confirmed": False})
            else:
                s.send({"type": "extension_ui_response", "id": f["id"], "cancelled": True})

    s.wait(prompt_done("p1"), 120, answer)


def scenario_permission(s, cwd, **_):
    negotiate(s)
    s.send({"id": "p1", "type": "prompt", "message": "Run the shell command `echo hi` with the bash tool and reply with its output."})
    seen = []

    def answer(f):
        if f.get("type") == "extension_ui_request" and f.get("method") not in ("setWidget", "setStatus", "notify", "setTitle", "set_editor_text"):
            seen.append(f)
            if f["method"] == "confirm":
                s.send({"type": "extension_ui_response", "id": f["id"], "confirmed": False})
            elif f["method"] == "select":
                s.send({"type": "extension_ui_response", "id": f["id"], "cancelled": True})
            else:
                s.send({"type": "extension_ui_response", "id": f["id"], "cancelled": True})

    s.wait(prompt_done("p1"), 120, answer)


def scenario_queue(s, cwd, **_):
    negotiate(s)
    s.send({"id": "p1", "type": "prompt", "message": "Count slowly from 1 to 30, one number per line, then stop."})
    s.wait(lambda f: f.get("type") == "agent_start", 30)
    s.send({"id": "q1", "type": "prompt", "message": "After that, say DONE.", "streamingBehavior": "followUp"})
    s.send({"id": "q2", "type": "steer", "message": "Stop counting at 5."})
    s.send({"id": "gs", "type": "get_state"})
    s.wait(prompt_done("p1"), 120)
    s.wait(settled, 120)


def scenario_btw(s, cwd, **_):
    negotiate(s)
    s.send({"id": "b1", "type": "btw", "question": "What is 2+2? Answer with one digit."})
    s.wait(is_resp("b1"), 90)
    s.send({"id": "bh", "type": "get_btw_history"})
    s.wait(is_resp("bh"), 10)


def scenario_subagent(s, cwd, **_):
    open(os.path.join(cwd, "a.txt"), "w").write("alpha\n")
    open(os.path.join(cwd, "b.txt"), "w").write("beta\n")
    negotiate(s)
    s.send({"id": "sub", "type": "set_subagent_subscription", "level": "events"})
    s.wait(is_resp("sub"), 10)
    s.send({"id": "p1", "type": "prompt", "message": "Use the task tool to launch one subagent that reads a.txt and b.txt and returns their contents. Reply with only the word OK."})
    s.wait(prompt_done("p1"), 240)
    s.send({"id": "gsa", "type": "get_subagents"})
    s.wait(is_resp("gsa"), 10)


def scenario_commands(s, cwd, **_):
    negotiate(s)
    for i, c in enumerate(["/jobs", "/session", "/usage", "/tools", "/context"]):
        s.send({"id": f"c{i}", "type": "prompt", "message": c})
        s.wait(prompt_done(f"c{i}"), 30)
    s.send({"id": "m", "type": "get_model_picker", "limit": 5})
    s.wait(is_resp("m"), 20)
    s.send({"id": "lp", "type": "get_login_providers"})
    s.wait(is_resp("lp"), 20)
    s.send({"id": "tl", "type": "get_available_thinking_levels"})
    s.wait(is_resp("tl"), 10)


def scenario_plan(spawn, cwd, **_):
    cfg = os.path.join(cwd, "plan-cfg.yml")
    open(cfg, "w").write("plan:\n  enabled: true\n  defaultOnStartup: true\n")
    ext = os.path.join(cwd, "plan-ext.ts")
    open(ext, "w").write(
        "export default function(pi: any) {\n"
        '  pi.registerCommand("check-plan-api", {\n'
        '    description: "Inspect plan APIs in rpc-ui",\n'
        "    handler: async (_args: string, ctx: any) => {\n"
        "      const info = {\n"
        "        hasSessionModes: Boolean(ctx.sessionModes),\n"
        "        pendingReview: ctx.getPendingPlanReview?.() ?? null,\n"
        "        openPlanReviewResult: await ctx.openPlanReview?.(),\n"
        '        resolveResult: ctx.resolvePlanReview?.("fake", "execute"),\n'
        "      };\n"
        '      ctx.ui?.notify?.("PLAN_API:" + JSON.stringify(info), "info");\n'
        "    }\n"
        "  });\n"
        "}\n"
    )
    s1 = spawn(["--no-session", "--config", cfg, "-e", ext])
    try:
        negotiate(s1)
        s1.send({"id": "c1", "type": "prompt", "message": "/check-plan-api"})
        s1.wait(prompt_done("c1"), 30)
        s1.send({"id": "p1", "type": "prompt", "message": "Reply with only OK."})
        s1.wait(prompt_done("p1"), 60)
        s1.wait(settled, 10)
    finally:
        s1.close()

    s2 = spawn(["--no-session", "--plan-yolo"])
    try:
        negotiate(s2)
        s2.send({"id": "p2", "type": "prompt", "message": "Reply with only OK."})
        s2.wait(prompt_done("p2"), 60)
        s2.wait(settled, 10)
    finally:
        s2.close()

    step = {"n": 0}

    class PlanHandler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get("content-length", "0"))
            body = self.rfile.read(length).decode("utf-8", errors="replace")
            try:
                model = json.loads(body).get("model", "")
            except ValueError:
                model = ""
            step["n"] += 1
            n = step["n"]
            if n == 1:
                blocks = [
                    {
                        "type": "tool_use",
                        "id": "toolu_plan_write",
                        "name": "write",
                        "input": {"path": "local://demo-plan.md", "content": "# Demo\n1. echo ok\n"},
                    }
                ]
                stop = "tool_use"
            elif n == 2:
                blocks = [
                    {
                        "type": "tool_use",
                        "id": "toolu_plan_propose",
                        "name": "bash",
                        "input": {"command": "propose demo"},
                    }
                ]
                stop = "tool_use"
            else:
                blocks = [{"type": "text", "text": "OK"}]
                stop = "end_turn"
            events = [
                (
                    "message_start",
                    {
                        "type": "message_start",
                        "message": {
                            "id": f"msg_p{n}",
                            "type": "message",
                            "role": "assistant",
                            "model": model,
                            "content": [],
                            "stop_reason": None,
                            "stop_sequence": None,
                            "usage": {"input_tokens": 10, "output_tokens": 1},
                        },
                    },
                )
            ]
            for idx, blk in enumerate(blocks):
                if blk["type"] == "tool_use":
                    events.append(
                        (
                            "content_block_start",
                            {
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": blk["id"],
                                    "name": blk["name"],
                                    "input": {},
                                },
                            },
                        )
                    )
                    events.append(
                        (
                            "content_block_delta",
                            {
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": {"type": "input_json_delta", "partial_json": json.dumps(blk["input"])},
                            },
                        )
                    )
                    events.append(("content_block_stop", {"type": "content_block_stop", "index": idx}))
                else:
                    events.append(
                        (
                            "content_block_start",
                            {"type": "content_block_start", "index": idx, "content_block": {"type": "text", "text": ""}},
                        )
                    )
                    events.append(
                        (
                            "content_block_delta",
                            {
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": {"type": "text_delta", "text": blk["text"]},
                            },
                        )
                    )
                    events.append(("content_block_stop", {"type": "content_block_stop", "index": idx}))
            events.append(
                (
                    "message_delta",
                    {
                        "type": "message_delta",
                        "delta": {"stop_reason": stop, "stop_sequence": None},
                        "usage": {"output_tokens": 5},
                    },
                )
            )
            events.append(("message_stop", {"type": "message_stop"}))
            sse = "".join(f"event: {ev}\ndata: {json.dumps(payload)}\n\n" for ev, payload in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(sse)
            self.wfile.flush()

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), PlanHandler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        s3 = spawn(
            [
                "--no-session",
                "--plan-yolo",
                "--model",
                "anthropic/claude-sonnet-4-5",
                "--plan-yolo-into",
                "anthropic/claude-haiku-4-5",
            ],
            env={"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "ANTHROPIC_API_KEY": "sk-ant-test-dummy"},
        )
        try:
            negotiate(s3)
            s3.send({"id": "p3", "type": "prompt", "message": "Write demo plan and propose it."})
            s3.wait(prompt_done("p3"), 30)
            s3.wait(settled, 10)
        finally:
            s3.close()
    finally:
        srv.shutdown()


def scenario_dialogs(spawn, cwd, **_):
    ext = os.path.join(cwd, "dialogs-ext.ts")
    open(ext, "w").write(
        "export default function(pi: any) {\n"
        '  pi.registerCommand("dialogs", {\n'
        '    description: "Exercise hook dialogs",\n'
        "    handler: async (_args: string, ctx: any) => {\n"
        '      const s = await ctx.ui.select("Pick a colour", ["red", "blue"]);\n'
        '      const c = await ctx.ui.confirm("Continue?", "Confirm action");\n'
        '      const t = await ctx.ui.input("Timed input", "placeholder", { timeout: 200 });\n'
        '      const sec = await ctx.ui.input("Secret input", "enter value", { secret: true });\n'
        '      const ed = await ctx.ui.editor("Edit note", "initial\\n");\n'
        '      const ab = await ctx.ui.input("Aborted input", "wait", { signal: AbortSignal.timeout(150) });\n'
        '      ctx.ui.notify("dialogs done:" + JSON.stringify({ s, c, t, sec, ed, ab }), "info");\n'
        "    }\n"
        "  });\n"
        "}\n"
    )
    s = spawn(["--no-session", "-e", ext])
    try:
        negotiate(s)
        s.send({"id": "d1", "type": "prompt", "message": "/dialogs"})

        def reply(f):
            if f.get("type") != "extension_ui_request":
                return
            m = f.get("method")
            if m == "select":
                s.send({"type": "extension_ui_response", "id": f["id"], "value": "blue"})
            elif m == "confirm":
                s.send({"type": "extension_ui_response", "id": f["id"], "confirmed": True})
            elif m == "input":
                if f.get("secret"):
                    s.send({"type": "extension_ui_response", "id": f["id"], "value": "demo-secret"})
                # Let timed and AbortSignal inputs remain unanswered so omp emits cancel{targetId}
            elif m == "editor":
                s.send({"type": "extension_ui_response", "id": f["id"], "value": "edited\n"})

        s.wait(prompt_done("d1"), 30, reply)
    finally:
        s.close()


def scenario_compaction(spawn, cwd, **_):
    sessions = os.path.join(cwd, ".sessions-cmp")
    os.makedirs(sessions, exist_ok=True)
    cfg = os.path.join(cwd, "cmp-cfg.yml")
    open(cfg, "w").write(
        "compaction:\n"
        "  enabled: true\n"
        "  modelThresholdsEnabled: false\n"
        "  thresholdTokens: 16500\n"
        "  keepRecentTokens: 1\n"
    )
    s = spawn(["--session-dir", sessions, "--config", cfg])
    try:
        negotiate(s)
        pad = "word " * 1800
        s.send({"id": "p1", "type": "prompt", "message": f"Reply only OK. Ignore: {pad}"})
        s.wait(prompt_done("p1"), 90)
        s.wait(settled, 30)
        s.send({"id": "p2", "type": "prompt", "message": "Say hello in 3 words."})
        s.wait(prompt_done("p2"), 60)
        s.wait(settled, 15)
        s.send({"id": "c1", "type": "prompt", "message": "/compact"})
        s.wait(lambda f: f.get("type") == "manual_compaction_end", 60)
        s.wait(lambda f: f.get("type") == "command_output", 10)
        s.send({"id": "st", "type": "get_state"})
        s.wait(is_resp("st"), 10)
    finally:
        s.close()


def scenario_retry(spawn, cwd, **_):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get("content-length", "0"))
            body = self.rfile.read(length).decode("utf-8", errors="replace")
            try:
                model = json.loads(body).get("model", "")
            except ValueError:
                model = ""
            if "sonnet" in model:
                self.send_response(429)
                self.send_header("Content-Type", "application/json")
                self.send_header("Retry-After", "65")
                err = json.dumps(
                    {"type": "error", "error": {"type": "rate_limit_error", "message": "Rate limit exceeded"}}
                ).encode()
                self.send_header("Content-Length", str(len(err)))
                self.end_headers()
                self.wfile.write(err)
                return
            sse = (
                "event: message_start\n"
                f'data: {{"type":"message_start","message":{{"id":"msg_ok","type":"message","role":"assistant","model":{json.dumps(model)},"content":[],"stop_reason":null,"stop_sequence":null,"usage":{{"input_tokens":10,"output_tokens":1}}}}}}\n\n'
                "event: content_block_start\n"
                'data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n'
                "event: content_block_delta\n"
                'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"OK"}}\n\n'
                "event: content_block_stop\n"
                'data: {"type":"content_block_stop","index":0}\n\n'
                "event: message_delta\n"
                'data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}\n\n'
                "event: message_stop\n"
                'data: {"type":"message_stop"}\n\n'
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(sse)
            self.wfile.flush()

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    cfg = os.path.join(cwd, "retry-cfg.yml")
    open(cfg, "w").write(
        "retry:\n"
        "  enabled: true\n"
        "  maxRetries: 3\n"
        "  baseDelayMs: 50\n"
        "  maxDelayMs: 5000\n"
        "  modelFallback: true\n"
        "  fallbackChains:\n"
        '    "anthropic/claude-sonnet-4-5":\n'
        '      - "anthropic/claude-haiku-4-5"\n'
    )
    try:
        s = spawn(
            ["--no-session", "--config", cfg, "--model", "anthropic/claude-sonnet-4-5"],
            env={"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "ANTHROPIC_API_KEY": "sk-ant-test-dummy"},
        )
        try:
            negotiate(s)
            s.send({"id": "p1", "type": "prompt", "message": "hi"})
            s.wait(prompt_done("p1"), 30)
            s.wait(settled, 10)
        finally:
            s.close()
    finally:
        srv.shutdown()


def scenario_chunk(spawn, cwd, **_):
    ext = os.path.join(cwd, "chunk-ext.ts")
    open(ext, "w").write(
        "export default function(pi: any) {\n"
        '  pi.registerCommand("bigmsg", {\n'
        '    description: "Emit a >1 MiB message so rpc_chunk triggers",\n'
        "    handler: async () => {\n"
        '      pi.sendMessage({ customType: "big", content: "B".repeat(1100000), display: true }, { triggerTurn: false });\n'
        "    }\n"
        "  });\n"
        "}\n"
    )
    s = spawn(["--no-session", "-e", ext])
    try:
        negotiate(s)
        saw_big = False

        def track(f):
            nonlocal saw_big
            if f.get("type") == "message_end" and f.get("message", {}).get("customType") == "big":
                saw_big = True

        s.send({"id": "m1", "type": "prompt", "message": "/bigmsg"})
        s.wait(prompt_done("m1"), 20, track)
        if not saw_big:
            s.wait(lambda f: f.get("type") == "message_end" and f.get("message", {}).get("customType") == "big", 20)
        s.send({"id": "pg", "type": "get_messages_page", "limit": 10})
        s.wait(is_resp("pg"), 20)
    finally:
        s.close()


def scenario_lease(spawn, cwd, **_):
    sdir1 = os.path.join(cwd, "s1")
    sdir2 = os.path.join(cwd, "s2")
    sdir3 = os.path.join(cwd, "s3")
    for d in (sdir1, sdir2, sdir3):
        os.makedirs(d, exist_ok=True)
    s1 = spawn(["--session-dir", sdir1])
    try:
        negotiate(s1)
        s1.send({"id": "p1", "type": "prompt", "message": "Reply with only OK."})
        s1.wait(prompt_done("p1"), 60)
        s1.wait(settled, 10)
        s1.send({"id": "st1", "type": "get_state"})
        sid = s1.wait(is_resp("st1"), 10)["data"]["sessionId"]
        s1.send({"id": "l1", "type": "get_session_lease", "sessionIds": [sid]})
        s1.wait(is_resp("l1"), 10)

        s2 = spawn(["--session-dir", sdir2])
        try:
            negotiate(s2)
            s2.send({"id": "l2", "type": "get_session_lease", "sessionIds": [sid]})
            s2.wait(is_resp("l2"), 10)
            s2.send({"id": "op2", "type": "open_session", "sessionDir": sdir1})
            s2.wait(is_resp("op2"), 10)
            s2.send({"id": "st2", "type": "get_state"})
            s2.wait(is_resp("st2"), 10)
        finally:
            s2.close()
    finally:
        s1.close()

    s3 = spawn(["--session-dir", sdir3])
    try:
        negotiate(s3)
        s3.send({"id": "l3", "type": "get_session_lease", "sessionIds": [sid]})
        s3.wait(is_resp("l3"), 10)
    finally:
        s3.close()


def scenario_worktree_fork(spawn, cwd, **_):
    wt1 = os.path.join(cwd, "wt1")
    wt2 = os.path.join(cwd, "wt2")
    sdir1 = os.path.join(cwd, "s1")
    sdir2 = os.path.join(cwd, "s2")
    sdir3 = os.path.join(cwd, "s3")
    sdir4 = os.path.join(cwd, "s4")
    for d in (wt1, sdir1, sdir2, sdir3, sdir4):
        os.makedirs(d, exist_ok=True)
    subprocess.run(["git", "init", "-q", wt1], check=True)
    subprocess.run(["git", "-C", wt1, "config", "user.email", "user@example.com"], check=True)
    subprocess.run(["git", "-C", wt1, "config", "user.name", "User"], check=True)
    open(os.path.join(wt1, "a.txt"), "w").write("wt1\n")
    subprocess.run(["git", "-C", wt1, "add", "."], check=True)
    subprocess.run(["git", "-C", wt1, "commit", "-q", "-m", "init"], check=True)
    subprocess.run(["git", "-C", wt1, "worktree", "add", "-q", "-b", "wt2", wt2], check=True)

    s1 = spawn(["--session-dir", sdir1], cwd=wt1)
    try:
        negotiate(s1)
        s1.send({"id": "p1", "type": "prompt", "message": "Reply with only OK."})
        s1.wait(prompt_done("p1"), 60)
        s1.wait(settled, 10)
        s1.send({"id": "st1", "type": "get_state"})
        sfile1 = s1.wait(is_resp("st1"), 10)["data"]["sessionFile"]
    finally:
        s1.close()

    s2 = spawn(["--session-dir", sdir2], cwd=wt2)
    try:
        negotiate(s2)
        s2.send({"id": "op2", "type": "open_session", "sessionDir": sdir1})
        s2.wait(is_resp("op2"), 10)
        s2.send({"id": "sw2", "type": "switch_session", "sessionPath": sfile1})
        s2.wait(is_resp("sw2"), 10)
        s2.send({"id": "st2", "type": "get_state"})
        s2.wait(is_resp("st2"), 10)
    finally:
        s2.close()

    s3 = spawn(["--cwd", wt2, "--session-dir", sdir3, "--resume", sfile1], cwd=wt2)
    try:
        negotiate(s3)
        s3.send({"id": "fk3", "type": "fork"})
        s3.wait(is_resp("fk3"), 10)
        s3.send({"id": "st3", "type": "get_state"})
        s3.wait(is_resp("st3"), 10)
        s3.send({"id": "bp3", "type": "bash", "command": "pwd"})
        s3.wait(is_resp("bp3"), 10)
    finally:
        s3.close()

    s4 = spawn(["--cwd", wt2, "--session-dir", sdir4, "--fork", sfile1], cwd=wt2)
    try:
        negotiate(s4)
        s4.send({"id": "st4", "type": "get_state"})
        s4.wait(is_resp("st4"), 10)
        s4.send({"id": "ses4", "type": "prompt", "message": "/session"})
        s4.wait(prompt_done("ses4"), 15)
        s4.send({"id": "bp4", "type": "bash", "command": "pwd"})
        s4.wait(is_resp("bp4"), 10)
    finally:
        s4.close()


SCENARIOS = {
    "plain": (scenario_plain, []),
    "ask": (scenario_ask, []),
    "permission": (scenario_permission, ["--approval-mode", "always-ask"]),
    "queue": (scenario_queue, []),
    "btw": (scenario_btw, []),
    "subagent": (scenario_subagent, []),
    "commands": (scenario_commands, []),
    "plan": (scenario_plan, None),
    "dialogs": (scenario_dialogs, None),
    "compaction": (scenario_compaction, None),
    "retry": (scenario_retry, None),
    "chunk": (scenario_chunk, None),
    "lease": (scenario_lease, None),
    "worktree_fork": (scenario_worktree_fork, None),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenario", choices=sorted(SCENARIOS))
    ap.add_argument("--omp", default="omp")
    ap.add_argument("--out")
    a = ap.parse_args()
    fn, extra = SCENARIOS[a.scenario]
    scratch = tempfile.mkdtemp(prefix="xctx-omp-rec-")
    real_scratch = os.path.realpath(scratch)
    sessions = os.path.join(scratch, ".sessions")
    os.makedirs(sessions)
    subprocess.run(["git", "init", "-q", scratch], check=False)
    lines = []
    log_lock = threading.Lock()

    def scrub(o):
        if isinstance(o, dict):
            out = {}
            for k, v in o.items():
                if k in ("providerPayload", "systemPrompt"):
                    continue
                if k == "dumpTools":
                    out[k] = [{"name": t.get("name")} for t in v if isinstance(t, dict)]
                    continue
                out[k] = scrub(v)
            return out
        if isinstance(o, list):
            return [scrub(x) for x in o]
        return o

    def log(line):
        head, body = line[:2], line[2:]
        try:
            body = json.dumps(scrub(json.loads(body)), separators=(",", ":"), ensure_ascii=False)
        except ValueError:
            pass
        line = head + body
        line = (
            line.replace(real_scratch, "/tmp/scratch")
            .replace(scratch, "/tmp/scratch")
            .replace(REAL_TMPDIR, "/tmp")
            .replace(TMPDIR, "/tmp")
            .replace(REAL_HOME, "/Users/USER")
            .replace(HOME, "/Users/USER")
        )
        line = re.sub(r"127\.0\.0\.1:\d+", "127.0.0.1:0", line)
        line = re.sub(r"sk-ant-[A-Za-z0-9_-]+", "TOKEN", line)
        if "@" in line:
            line = re.sub(r"(?<![\w.+-])[\w.+-]+@(?!marketplace)[\w-]+(?:\.[\w-]+)+", "user@example.com", line)
        if "." in line and '"type":"rpc_chunk"' not in line:
            line = re.sub(r"(?<![A-Za-z0-9_-])[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}", "JWT", line)
        with log_lock:
            lines.append(line)

    def spawn(args, cwd=scratch, env=None):
        return Session(a.omp, args, cwd, log, env=env)

    try:
        if extra is None:
            fn(spawn, scratch)
        else:
            s = Session(a.omp, ["--session-dir", sessions, *extra], scratch, log)
            try:
                fn(s, scratch)
            finally:
                s.close()
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    out = a.out or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", a.scenario + ".jsonl")
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"{a.scenario}: {len(lines)} frames -> {out}")


if __name__ == "__main__":
    sys.exit(main())
