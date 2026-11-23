# Spaces

**A living, task-first workspace for building software with teams of agents.**

Development moved from writing code, to chatting with an agent, to working at the level of **ideas and tasks**. You say what should change—by voice, prompt, or canvas—and the system handles the repos, worktrees, environments, tools, and agent coordination behind it.

![Spaces overview](./spaces.png)

---

## Core Ideas

### 1. Think in tasks, not repos or branches
You describe the outcome you want (*"make checkout retry safe when we rate limit"*), not which repo, branch, or worktree to open. The system searches your code, figures out which projects are involved, spins up hidden worktrees behind the scenes, and opens a **space** for the task.

Do we even need a file-tree sidebar? This starts from the ground up: the unit of work is the task, not the directory tree.

### 2. A living system of spaces, not a fixed IDE
Workspaces are called **spaces**. There are no fixed pages or numbered steps:
- **Born** from a task (text, voice, or canvas).
- **Shaped** to fit the work—a frontend task, a backend task, a full-stack feature, and a bug hunt each look different.
- **Reshaped** in place when the work turns (a failing test turns a dev space into a debug space without losing context).
- **Dissolved** once reviewed and shipped—PRs opened, ticket updated, worktrees removed—while every other space keeps living.

### 3. Git-branch your entire agent
What if you could branch not just files, but an entire running agent—its **sandbox, storage, and memory**?
- Fork a live session to try something risky without polluting the main line.
- Run competing attempts in parallel (Solutions 1, 2, 3), each in its own hidden worktree.
- Compare **outcomes** (repro, tests, diffs), not git branches—discard the dead ends, or merge the winner back through a review sheet.
- Keep a real **session tree** with recall and a session-management layer so nothing gets lost, plus `/tmp` scratch storage for intermediate artifacts.

### 4. Parent agent, orchestrators, and multi-agent graphs
Multi-agent first, with a clear hierarchy you can inspect and steer at any time:
- **Parent agent (always on):** the single agent that controls the entire UI and dashboard. It turns tasks into spaces, maps tasks to repos, mounts plugins, reshapes panes, and owns the inbox. It writes no code.
- **Orchestrator (one per space):** splits the task across subagents, holds the contract between them, and brings anything that widens the space back to you.
- **Subagents (one writer per repo + read-only helpers):**
  - Full-stack work runs a backend agent and a frontend agent together.
  - Multi-service work runs one agent per service (e.g. `orders-svc` and `inventory-svc`).
  - Helpers (tests, reviewer) read across repos without write access.
- **Inter-service communication:** writers never edit each other's repo. They talk over a space channel and agree on a shared **contract** (API shape, event payload, or schema) that the orchestrator holds and the test agent verifies on both sides.
- **You control the graph:** pause or resume any agent, add or remove lanes, talk directly to a specific agent, split a writer into its own space, or merge spaces together.

### 5. A harness that shapes both structure and flow
```
space -> repos -> capabilities & environment -> tools
```
The harness is what lets the parent agent reshape an entire space—not just its layout, but how agents work inside it:
- **Shape:** mount, arrange, or close panes (`layout.set`, `pane.mount`, `pane.close`).
- **Flow:** wire up multi-agent loops (`write -> test -> probe -> review`) around a **shared space** (contract, findings, notes, artifacts) so agents can work independently while you step away.
- **Autonomy levels:** *ask each step*, *within budget*, or *until checks pass*.
- **Capabilities & plan gate:** nothing runs until you launch, and widening a space—adding agents, repos, budget, or production access—always returns to the plan gate.
- **Harness primitives:** code mode for batching tool calls and session ops, pluggable agent runners (`pi`, `fx`), and repo harness artifacts (`AGENTS.md`, `init.sh`, `progress.md`).

### 6. Connectors and plugins as live panes + agent tools
A space shouldn't be locked to text and a terminal. Through a plugin/connector system, the parent agent pulls in whatever the task calls for:
- **Browser** for UI changes.
- **Postman-style API client** for endpoints and request replays.
- **Logs** and **database** (read-only until writes are approved) for debugging and data work.
- **Jira** (with the space's plan and sessions attached), **Figma**, or custom plugin manifests.

Every plugin provides three things: a **pane** you can see, **tools** agents can call without you watching, and **events** that can trigger or stop a loop.

### 7. Isolated environments with `mise`
Each space runs inside its own [mise environment](https://mise.jdx.dev/environments/) (`MISE_ENV=<space>`), layered on top of each repo's `mise.toml` (with `mise.local.toml` still winning locally):
- **No clashes:** concurrent spaces on the same repos get separate ports, tools, and variables while leaving your own shell untouched.
- **Required vars:** missing `required` variables halt the agent and land in the inbox with mise's own help text.
- **Redacted secrets:** secret values stay masked in the channel, shared notes, and task output.
- **Gated prod:** switching to `MISE_ENV=production` returns to the plan gate.

---

## What's in This Folder

| File | Description |
|---|---|
| `spaces.excalidraw` | The 17-frame architecture and TUI design (open in Excalidraw) |
| `spaces.png` | Full canvas render of `spaces.excalidraw` |
| `gen.mjs` / `lib.mjs` | Script that generates `spaces.excalidraw` |
| `../tui/spaces.mjs` | Runnable terminal prototype of the living system |

### Run the terminal prototype

```bash
cd ..
node tui/spaces.mjs          # interactive (g graph, m plugins, e mise env, o autonomy, n new task)
node tui/spaces.mjs --demo   # prints every stage of the lifecycle
```

### Regenerate the drawing

```bash
node gen.mjs > spaces.excalidraw
excalidraw render spaces.excalidraw --out spaces.png
```
