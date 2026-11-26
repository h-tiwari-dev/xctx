#!/usr/bin/env python3
"""Record an omp `--mode rpc-ui` conversation as a fixture.

Usage: record.py SCENARIO [--omp PATH] [--out FILE]

Every frame is written verbatim, one per line, prefixed by its direction: `< ` for a frame omp wrote, `> ` for a
command or UI response sent to it. The home directory is replaced by `/Users/USER`, and the scratch directory by
`/tmp/scratch`. Each scenario runs omp in a fresh temporary directory with its own `--session-dir`, never in a
real repository, and never logs in or approves anything on the user's behalf beyond what the scenario states.
"""
import argparse
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time

HOME = os.path.expanduser("~")


class Session:
    def __init__(self, omp, args, cwd, log):
        self.log = log
        self.q = queue.Queue()
        self.p = subprocess.Popen(
            [omp, "--mode", "rpc-ui", "--no-title", *args],
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            line = line.rstrip("\n")
            self.log("< " + line)
            try:
                self.q.put(json.loads(line))
            except ValueError:
                pass
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


SCENARIOS = {
    "plain": (scenario_plain, []),
    "ask": (scenario_ask, []),
    "permission": (scenario_permission, ["--approval-mode", "always-ask"]),
    "queue": (scenario_queue, []),
    "btw": (scenario_btw, []),
    "subagent": (scenario_subagent, []),
    "commands": (scenario_commands, []),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenario", choices=sorted(SCENARIOS))
    ap.add_argument("--omp", default="omp")
    ap.add_argument("--out")
    a = ap.parse_args()
    fn, extra = SCENARIOS[a.scenario]
    scratch = tempfile.mkdtemp(prefix="xctx-omp-rec-")
    sessions = os.path.join(scratch, ".sessions")
    os.makedirs(sessions)
    subprocess.run(["git", "init", "-q", scratch], check=False)
    lines = []

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
        line = line.replace(scratch, "/tmp/scratch").replace(HOME, "/Users/USER")
        line = re.sub(r"[\w.+-]+@(?!marketplace)[\w-]+(?:\.[\w-]+)+", "user@example.com", line)
        line = re.sub(r"[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}", "JWT", line)
        lines.append(line)

    s = Session(a.omp, ["--session-dir", sessions, *extra], scratch, log)
    try:
        fn(s, scratch)
    finally:
        s.close()
    out = a.out or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", a.scenario + ".jsonl")
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"{a.scenario}: {len(lines)} frames -> {out}")


if __name__ == "__main__":
    sys.exit(main())
