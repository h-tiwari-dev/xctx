// The living system: you give tasks, a parent agent runs the dashboard and opens
// spaces, each space has an orchestrator that runs one subagent per repo, and the
// subagents talk to each other through a contract.
// Usage: node gen.mjs > spaces.excalidraw
import { C, text, rect, arrow, note, frame as place, scene } from "./lib.mjs";

Object.assign(C, {
  ink: "#14161c",
  stroke: "#e7e2d6",
  dim: "#938e82",
  faint: "#343a46",
  green: "#8fbf7a",
  amber: "#e0b15a",
  violet: "#b3a6e0",
  red: "#e08b7a",
  accent: "#8eb4d4",
});

const FW = 1600;
const FH = 1000;
// Frames are laid out two per row in the order they appear in this file.
let count = 0;
const at = () => [0, 0];
function frame(name, _x, _y, w, h, draw) {
  const i = count++;
  place(`${i} ${name.replace(/^\d+ /, "")}`, (i % 2) * (FW + 140), Math.floor(i / 2) * (FH + 140), w, h, draw);
}
const TX = 40;
const TY = 110;
const TW = 1060;
const TH = 760;

function header(X, Y, title, subtitle) {
  text(X + 40, Y + 24, title, { size: 26 });
  note(X + 40, Y + 66, subtitle);
}

function node(x, y, w, h, title, body, c, o = {}) {
  rect(x, y, w, h, { stroke: c, strokeWidth: o.dashed ? 1 : 2, dashed: o.dashed });
  text(x + 16, y + 12, title, { size: o.size ?? 18, color: c });
  if (body) note(x + 16, y + 44, body, C.stroke);
}

function terminal(X, Y, { tabs, current, status, prompt }) {
  const x = X + TX, y = Y + TY;
  rect(x, y, TW, TH, { stroke: C.stroke, strokeWidth: 2, round: false });
  let tx = x + 14;
  tabs.forEach(([name, shape, color]) => {
    const label = shape ? `${name} [${shape}]` : name;
    const w = label.length * 8.6 + 20;
    const on = name === current;
    rect(tx, y + 10, w, 28, { stroke: on ? color : C.faint, bg: on ? "#232833" : undefined, round: false });
    text(tx + 10, y + 16, label, { size: 14, mono: true, color: on ? color : C.dim });
    tx += w + 8;
  });
  rect(x, y + 48, TW, 1, { stroke: C.faint, round: false });
  rect(x, y + TH - 70, TW, 1, { stroke: C.faint, round: false });
  text(x + 14, y + TH - 60, prompt || "> type a task, or n for a new one", { size: 14, mono: true, color: C.dim });
  rect(x, y + TH - 34, TW, 1, { stroke: C.faint, round: false });
  text(x + 14, y + TH - 26, status, { size: 13, mono: true, color: C.dim });
  return { x: x + 12, y: y + 60, w: TW - 24, h: TH - 140 };
}

function pane(x, y, w, h, title, lines = [], color = C.faint) {
  rect(x, y, w, h, { stroke: color, round: false });
  text(x + 10, y + 8, title, { size: 14, mono: true, color: color === C.faint ? C.stroke : color });
  lines.forEach(([l, c], i) => text(x + 10, y + 36 + i * 22, l, { size: 13, mono: true, color: c || C.dim }));
}

function side(X, Y, items) {
  let y = Y + TY;
  items.forEach(([t, b, c]) => {
    text(X + 1140, y, t, { size: 18, color: c || C.stroke });
    note(X + 1140, y + 30, b, C.stroke);
    y += 34 + b.split("\n").length * 19 + 34;
  });
}

const TABS = [["home", null, C.stroke], ["order sync", "dev", C.green], ["checkout 500", "debug", C.red]];
const withRate = (shape, color) => [...TABS, ["rate limit", shape, color]];

// The tree every space shows: parent, orchestrator, subagents.
function tree(states, sel) {
  const rows = [
    ["parent agent      runs this dashboard", C.violet],
    ["└ orchestrator    rate limit", C.accent],
  ];
  const agents = [
    ["be-agent  ", "payments    ", C.amber],
    ["fe-agent  ", "checkout-web", C.green],
    ["test-agent", "reads both  ", C.stroke],
    ["reviewer  ", "reads diffs ", C.stroke],
    ...(states.length > 4 ? [["docs-agent", "docs-site   ", C.stroke]] : []),
  ];
  agents.forEach(([n, r, c], i) => {
    const b = i === agents.length - 1 ? "└" : "├";
    rows.push([`${i === sel ? "›" : " "} ${b} ${n}  ${r}  ${states[i]}`, c]);
  });
  return rows;
}

const CHANNEL = [
  ["orch                split: payments owns the change, checkout-web consumes it", C.accent],
  ["be-agent → orch     payments API changes. drafting contract v2", C.amber],
  ["orch → fe-agent     contract v2 is coming. wait for it, don't guess the API", C.accent],
  ["fe-agent → be-agent need the error shape, not just the status code", C.green],
  ["be-agent → fe-agent { code, retryAfter } added to contract v2", C.amber],
  ["orch                contract v2 agreed by both. fe-agent unblocked", C.accent],
  ["test-agent          contract test passes on both sides", C.stroke],
];

// 0. The system
frame("0 The living system", ...at(0), FW, FH, (X, Y) => {
  header(X, Y, "One living system, run by a parent agent",
    "You give tasks. Spaces are born from them, change shape with the task, and dissolve after they ship. Many live at once.");
  node(X + 600, Y + 130, 400, 110, "parent agent", "runs the dashboard. makes, reshapes,\nand closes spaces. writes no code.", C.violet);
  node(X + 60, Y + 370, 300, 110, "born", "a task arrives. the parent finds\nthe repos and drafts a graph.", C.amber);
  node(X + 440, Y + 370, 300, 110, "shaped", "launch. agents start. panes\nare laid out for dev or debug.", C.green);
  node(X + 820, Y + 370, 300, 110, "reshaped", "the task changed. same space,\nsame agents, new panes.", C.red);
  node(X + 1200, Y + 370, 340, 110, "dissolved", "reviewed inside, shipped.\nworktrees removed.", C.violet);
  arrow(X + 700, Y + 245, X + 210, Y + 365, { stroke: C.faint });
  arrow(X + 760, Y + 245, X + 590, Y + 365, { stroke: C.faint });
  arrow(X + 840, Y + 245, X + 970, Y + 365, { stroke: C.faint });
  arrow(X + 900, Y + 245, X + 1370, Y + 365, { stroke: C.faint });
  arrow(X + 364, Y + 425, X + 436, Y + 425, { stroke: C.dim });
  arrow(X + 744, Y + 410, X + 816, Y + 410, { stroke: C.dim });
  arrow(X + 816, Y + 445, X + 744, Y + 445, { stroke: C.dim, dashed: true });
  arrow(X + 1124, Y + 425, X + 1196, Y + 425, { stroke: C.dim });
  note(X + 690, Y + 500, "a space can reshape\nany number of times", C.red);

  rect(X + 60, Y + 600, 1480, 330, { stroke: C.faint });
  text(X + 80, Y + 620, "What stays the same on every screen", { size: 20 });
  [
    ["top", "the tab bar is the set of living spaces, with their current shape. Not an order."],
    ["middle", "panes for this space, laid out by the parent agent. The agent graph is always one key away."],
    ["prompt", "one input: type or say a task, or talk to any agent in the space."],
    ["bottom", "status: agents, contract, cost. Keys: tab next space, n new task, g graph."],
  ].forEach(([k, v], i) => {
    text(X + 80, Y + 680 + i * 56, k, { size: 17, color: C.accent });
    text(X + 200, Y + 680 + i * 56, v, { size: 17 });
  });
});

// 1. The agent graph
frame("1 The agent graph", ...at(1), FW, FH, (X, Y) => {
  header(X, Y, "Who does what: parent, orchestrators, subagents",
    "One parent for the whole dashboard. One orchestrator per space. One writer per repo, plus read-only helpers.");
  node(X + 700, Y + 112, 200, 56, "you", null, C.stroke);
  node(X + 520, Y + 220, 560, 100, "parent agent", "runs the dashboard. turns tasks into spaces, finds the repos,\nlays out panes, owns the inbox. writes no code.", C.violet);
  arrow(X + 800, Y + 170, X + 800, Y + 216, { stroke: C.dim });

  const spaceBox = (x, label, c, o, subs, link, trees) => {
    rect(x, Y + 380, 700, 560, { stroke: c, dashed: true });
    text(x + 20, Y + 392, label, { size: 17, color: c });
    node(x + 100, Y + 440, 500, 90, "orchestrator", "splits the task, holds the contract,\nbrings new lanes back to you", C.accent);
    subs.forEach(([t, b, sc, dashed], i) => {
      const sx = x + 30 + i * 220;
      node(sx, Y + 600, 200, 96, t, b, sc, { dashed });
      arrow(x + 350, Y + 532, sx + 100, Y + 596, { stroke: C.dim });
    });
    rect(x + 150, Y + 740, 180, 40, { stroke: C.stroke, dashed: true, label: link, labelSize: 14 });
    arrow(x + 130, Y + 698, x + 200, Y + 738, { stroke: C.stroke, dashed: true, head: false });
    arrow(x + 350, Y + 698, x + 280, Y + 738, { stroke: C.stroke, dashed: true, head: false });
    trees.forEach(([t, w], i) => {
      rect(x + 30 + i * 330, Y + 820, w, 34, { stroke: C.faint, round: false });
      text(x + 42 + i * 330, Y + 828, t, { size: 13, mono: true, color: C.dim });
    });
    note(x + 30, Y + 872, "hidden worktrees, one per writer. you never pick them.", C.dim);
    arrow(X + 800, Y + 322, x + 350, Y + 436, { stroke: C.dim });
  };
  spaceBox(X + 60, "space: rate limit   full stack", C.green, null, [
    ["be-agent", "payments\nwrites", C.amber],
    ["fe-agent", "checkout-web\nwrites", C.green],
    ["helpers", "test-agent, reviewer\nread only", C.stroke, true],
  ], "contract v2", [["payments  agent/be-agent", 300], ["checkout-web  agent/fe-agent", 320]]);
  spaceBox(X + 840, "space: order sync   two services", C.amber, null, [
    ["orders-agent", "orders-svc\nwrites", C.amber],
    ["inventory-agent", "inventory-svc\nwrites", C.green],
    ["helpers", "test-agent, reviewer\nread only", C.stroke, true],
  ], "stock.reserved", [["orders-svc  agent/orders", 300], ["inventory-svc  agent/inv", 320]]);
  note(X + 60, Y + 955, "solid: who directs whom.   dashed: subagents talking to each other through a contract the orchestrator holds.", C.dim);
});

// 2. Task in, repos out
frame("2 Task in, repos out", ...at(2), FW, FH, (X, Y) => {
  header(X, Y, "You think in tasks. The parent agent finds the repos.",
    "No repo picker, no branch picker. What should change goes in; a space with the right agents comes out.");
  pane(X + 60, Y + 140, 420, 400, "you say", [
    ["> make checkout retry safe", C.stroke],
    ["  when we rate limit", C.stroke],
    ["", null],
    ["that's it. no repo, no branch,", C.dim],
    ["no worktree, no agent names.", C.dim],
  ], C.stroke);
  pane(X + 560, Y + 140, 460, 400, "the parent agent works out", [
    ["1 reads the task", C.stroke],
    ["2 searches the code index", C.stroke],
    ["    POST /checkout  → payments", C.amber],
    ["    Checkout.tsx    → checkout-web", C.green],
    ["3 picks a shape: dev", C.stroke],
    ["4 checks living spaces for overlap", C.stroke],
    ["    none. opens a new space", C.dim],
    ["5 hands it to an orchestrator", C.stroke],
  ], C.violet);
  pane(X + 1100, Y + 140, 440, 400, "the space gets", [
    ["be-agent    payments", C.amber],
    ["fe-agent    checkout-web", C.green],
    ["test-agent  reads both", C.stroke],
    ["reviewer    reads diffs", C.stroke],
    ["", null],
    ["contract    payments ↔ checkout-web", C.stroke],
    ["worktrees   one per writer", C.dim],
    ["mise env   one per space, ports never clash", C.dim],
  ], C.accent);
  arrow(X + 484, Y + 340, X + 556, Y + 340, { stroke: C.dim });
  arrow(X + 1024, Y + 340, X + 1096, Y + 340, { stroke: C.dim });

  rect(X + 60, Y + 600, 1480, 340, { stroke: C.faint });
  text(X + 90, Y + 625, "You think about", { size: 20, color: C.stroke });
  text(X + 820, Y + 625, "The system handles", { size: 20, color: C.accent });
  ["what should change", "whether the result is right", "approving anything that widens a space", "talking to an agent when you want to steer it"]
    .forEach((t, i) => text(X + 90, Y + 680 + i * 48, t, { size: 17 }));
  ["which repos and files the task touches", "worktrees, branches, ports, env", "which agent talks to which, and the contract", "merging, opening PRs, cleaning up"]
    .forEach((t, i) => text(X + 820, Y + 680 + i * 48, t, { size: 17 }));
});

// The harness
frame("The harness", ...at(), FW, FH, (X, Y) => {
  header(X, Y, "The harness: what the parent agent can change",
    "The parent is always on and is the only agent that controls the UI. The harness gives it three levers, and plugins to pull in.");
  node(X + 700, Y + 108, 200, 50, "you", null, C.stroke);
  node(X + 420, Y + 196, 760, 110, "parent agent   always on",
    "the only agent that controls the UI. reads every space, mounts plugins,\nshapes panes and loops, and asks you before a space gets wider.", C.violet);
  arrow(X + 800, Y + 160, X + 800, Y + 192, { stroke: C.dim });
  arrow(X + 800, Y + 308, X + 800, Y + 356, { stroke: C.dim });

  rect(X + 60, Y + 360, 1480, 230, { stroke: C.accent });
  text(X + 80, Y + 372, "harness", { size: 18, color: C.accent });
  node(X + 100, Y + 420, 440, 140, "shape", "which panes a space has, and where.\nlayout.set  pane.mount  pane.close", C.green);
  node(X + 580, Y + 420, 440, 140, "flow", "loops, gates, budgets, autonomy.\nloop.start  gate.ask  budget.set", C.amber);
  node(X + 1060, Y + 420, 440, 140, "capabilities", "what each agent may touch, and where.\na mise env per space. production\ncomes back to the plan gate.", C.red);
  arrow(X + 800, Y + 592, X + 800, Y + 640, { stroke: C.dim });

  text(X + 60, Y + 650, "connectors and plugins", { size: 18, color: C.violet });
  [
    ["browser", "page.open"], ["api client", "http.send"], ["terminal", "shell.run"], ["logs", "logs.query"],
    ["database", "sql.read"], ["jira", "issue.read"], ["figma", "frame.read"], ["your own", "manifest"],
  ].forEach(([t, b], i) => node(X + 60 + i * 185, Y + 690, 170, 90, t, b, C.violet, { dashed: i === 7, size: 16 }));
  note(X + 60, Y + 820, "Every plugin brings three things: a pane you can see, tools agents can call, and events that can start or stop a loop.", C.stroke);
  note(X + 60, Y + 850, "The harness is what lets a space look like a browser one minute and a Postman-style API client the next.", C.dim);
  note(X + 60, Y + 880, "Agents can use plugins with nobody watching, so a space can keep working on its own.", C.dim);
});

// A plugin, and how a space is composed
frame("Plugins compose a space", ...at(), FW, FH, (X, Y) => {
  header(X, Y, "A space is agents + plugins + a loop + shared state",
    "The parent composes it from the task, and recomposes it when the task changes. You can change any part.");
  pane(X + 60, Y + 130, 620, 400, "plugin manifest   api-client", [
    ["plugin      api-client", C.stroke],
    ["pane        request | response | history", C.stroke],
    ["tools       http.send  http.replay  http.diff", C.green],
    ["events      response.status  response.slow", C.amber],
    ["scope       local, staging. never production", C.red],
    ["", null],
    ["mounted by  the parent agent, or you", C.dim],
    ["used by     be-agent, test-agent, reviewer", C.dim],
    ["", null],
    ["connectors are the same shape, pointed at an", C.dim],
    ["outside service: jira, figma, slack, a database.", C.dim],
  ], C.violet);
  const row = (y, label, c, items) => {
    text(X + 760, y, label, { size: 17, color: c });
    let x = X + 920;
    items.forEach((t) => {
      const w = t.length * 8.4 + 26;
      rect(x, y - 6, w, 32, { stroke: c, round: false });
      text(x + 12, y, t, { size: 14, mono: true, color: C.stroke });
      x += w + 10;
    });
  };
  rect(X + 740, Y + 130, 800, 400, { stroke: C.faint });
  text(X + 760, Y + 146, "space: rate limit", { size: 18, color: C.green });
  row(Y + 210, "agents", C.accent, ["parent", "orchestrator", "be-agent", "fe-agent", "test-agent"]);
  row(Y + 280, "plugins", C.violet, ["terminal", "browser", "api client"]);
  row(Y + 350, "loop", C.amber, ["write → test → probe → review"]);
  row(Y + 420, "shared", C.stroke, ["contract v2", "findings", "notes", "artifacts"]);
  note(X + 760, Y + 480, "shared state is what lets agents work apart and still agree.", C.dim);

  rect(X + 60, Y + 580, 1480, 360, { stroke: C.faint });
  text(X + 80, Y + 600, "What the parent mounts, from the task", { size: 20 });
  [
    ["building UI", "browser, terminal", C.green],
    ["an API or a service", "api client, logs", C.accent],
    ["a bug", "logs, api client, repro", C.red],
    ["a data change", "database, read only until you approve writes", C.amber],
    ["a ticket in the task", "jira, so agents read it and post back", C.violet],
  ].forEach(([k, v, c], i) => {
    text(X + 80, Y + 656 + i * 52, k, { size: 17, color: c });
    text(X + 400, Y + 656 + i * 52, v, { size: 17 });
  });
});

// mise environments
frame("A mise environment per space", ...at(), FW, FH, (X, Y) => {
  header(X, Y, "Each space runs inside its own mise environment",
    "The repo's mise.toml sets tools and variables. The space adds its own file, so two spaces on one repo never share a port or a secret.");
  pane(X + 60, Y + 130, 480, 470, "payments/mise.toml", [
    ["[tools]", C.dim],
    ["node = \"24\"", C.stroke],
    ["pnpm = \"10\"", C.stroke],
    ["", null],
    ["[env]", C.dim],
    ["NODE_ENV = \"development\"", C.stroke],
    ["DATABASE_URL = { required = true }", C.amber],
    ["_.file = \".env\"", C.stroke],
  ], C.stroke);
  pane(X + 560, Y + 130, 500, 470, "payments/mise.rate-limit.toml", [
    ["MISE_ENV=rate-limit", C.accent],
    ["", null],
    ["[env]", C.dim],
    ["PORT = \"4001\"", C.green],
    ["DATABASE_URL = \"from the secret manager\"", C.stroke],
    ["", null],
    ["this file wins over mise.toml.", C.dim],
    ["mise.local.toml still wins over it.", C.dim],
    ["", null],
    ["checkout-web gets the same treatment,", C.dim],
    ["with VITE_API pointed at this port.", C.dim],
  ], C.accent);
  pane(X + 1080, Y + 130, 460, 470, "what an agent actually runs", [
    ["$ mise exec -- pnpm test", C.stroke],
    ["", null],
    ["node 24    pnpm 10", C.green],
    ["NODE_ENV=development", C.stroke],
    ["PORT=4001", C.stroke],
    ["DATABASE_URL=••••••••", C.amber],
    ["", null],
    ["tasks, shims and the terminal plugin", C.dim],
    ["all see this. your own shell does not.", C.dim],
  ], C.green);

  rect(X + 60, Y + 640, 1480, 300, { stroke: C.faint });
  text(X + 80, Y + 660, "Why a space gets its own", { size: 20 });
  [
    ["isolated", "order sync can take PORT=4100 while rate limit takes 4001, on the same repos."],
    ["required", "a missing DATABASE_URL stops the agent and lands in the inbox, with mise's own help text."],
    ["redacted", "secrets are masked in the channel, in shared notes, and in task output."],
    ["gated", "MISE_ENV=production is a different file, and switching to it comes back to the plan gate."],
  ].forEach(([k, v], i) => {
    text(X + 80, Y + 720 + i * 48, k, { size: 17, color: C.accent });
    text(X + 260, Y + 720 + i * 48, v, { size: 17 });
  });
});

// 3. Home
frame("3 Home", ...at(3), FW, FH, (X, Y) => {
  header(X, Y, "Home: every living space, and what each needs from you", "Each row is a whole agent graph. You see repos, but you never had to choose them.");
  const b = terminal(X, Y, { tabs: TABS, current: "home", status: "2 spaces alive   inbox 2   $3.94 today   tab next space   n new task" });
  pane(b.x, b.y, b.w, 260, "living spaces", [
    ["  order sync     dev     4 agents   orders-svc ↔ inventory-svc", C.green],
    ["  checkout 500   debug   4 agents   payments ↔ checkout-web", C.red],
    ["", null],
    ["  you say what should change. the parent agent finds the repos.", C.dim],
  ]);
  pane(b.x, b.y + 280, b.w, b.h - 280, "inbox", [
    ["  order sync     inventory-agent wants to run a migration     [a]pprove [d]eny", C.amber],
    ["  checkout 500   attempt 1: tests 42/42, repro passes         [enter] review", C.green],
  ]);
  side(X, Y, [
    ["Not a launcher", "Home lists what is alive.\nA new task starts from the\nprompt on any screen.", C.amber],
    ["Inbox", "Anything any agent needs from\nyou, across every space.", C.stroke],
  ]);
});

// 4. Born
frame("4 A space is born", ...at(4), FW, FH, (X, Y) => {
  header(X, Y, "A space is born: the parent agent drafts the graph", "You typed a task. It found two repos and proposes who works where. Nothing runs until you launch.");
  const b = terminal(X, Y, {
    tabs: withRate("new", C.amber), current: "rate limit",
    status: "not launched   $0.00 / $5   enter launch   d / v propose debug or dev",
    prompt: "> add rate limiting to the checkout api",
  });
  pane(b.x, b.y, 400, b.h, "what the parent agent found", [
    ["it touches", C.stroke],
    ["  payments        backend", C.amber],
    ["  checkout-web    frontend", C.green],
    ["", null],
    ["each writer gets its own worktree.", C.dim],
    ["you never pick a repo or branch.", C.dim],
    ["", null],
    ["not    production", C.red],
    ["shape  dev (proposed)", C.green],
  ], C.amber);
  pane(b.x + 416, b.y, b.w - 416, b.h, "the graph it will run", [
    ...tree(["idle", "idle", "idle", "idle"]),
    ["", null],
    ["one writer per repo. helpers read only.", C.dim],
    ["writers talk through a contract the orchestrator", C.dim],
    ["holds. nobody edits another agent's repo.", C.dim],
  ]);
  side(X, Y, [
    ["Born in place", "A new tab appears beside the\nothers. You don't leave the\nspace you were in.", C.amber],
    ["You can edit it", "Drop a lane, add one, or\nchange the shape before\nanything runs.", C.green],
  ]);
});

// 5. Dev
frame("5 Shaped as dev", ...at(5), FW, FH, (X, Y) => {
  header(X, Y, "Launched: backend and frontend work together", "Both writers are visible, with the channel between them. The frontend waits for the contract instead of guessing.");
  const b = terminal(X, Y, { tabs: withRate("dev", C.green), current: "rate limit", status: "4 agents   1 writing   contract draft   $0.62   g graph   b bug now   r review" });
  pane(b.x, b.y, 410, b.h, "agents", tree(["writing", "waiting on contract", "watching", "idle"]));
  const rx = b.x + 426, rw = b.w - 426, half = (rw - 16) / 2;
  pane(rx, b.y, half, 300, "be-agent   payments", [
    ["+ import { limit } from './limit'", C.green],
    ["+ router.use('/checkout', limit(10))", C.green],
    ["+ res.set('Retry-After', secs)", C.green],
    ["", null],
    ["implements contract v2", C.stroke],
  ], C.amber);
  pane(rx + half + 16, b.y, half, 300, "fe-agent   checkout-web", [
    ["waiting on contract v2", C.amber],
    ["not guessing the API", C.dim],
  ], C.green);
  pane(rx, b.y + 316, rw, b.h - 316, "channel   agents talking", CHANNEL.slice(0, 3), C.accent);
  side(X, Y, [
    ["Working together", "Two writers in two repos at\nonce, each in its own\nhidden worktree.", C.green],
    ["Blocked, not guessing", "fe-agent waits for the contract\nbefore it calls the new API.", C.amber],
  ]);
});

// 6. Graph + channel
frame("6 Agents talking", ...at(6), FW, FH, (X, Y) => {
  header(X, Y, "The graph view: agents talking through a contract", "Press g in any space. The orchestrator holds the boundary; the subagents agree on it.");
  const b = terminal(X, Y, { tabs: withRate("dev", C.green), current: "rate limit", status: "j/k pick   t talk   p pause   x remove helper   a add agent   g back" });
  pane(b.x, b.y, 640, 230, "graph   you control this", tree(["writing", "writing", "watching", "idle"], 0), C.violet);
  pane(b.x + 656, b.y, b.w - 656, 230, "contract", [
    ["payments ↔ checkout-web", C.stroke],
    ["v2 agreed by both", C.green],
    ["POST /checkout", C.dim],
    ["+ 429 { code, retryAfter }", C.green],
    ["+ Retry-After: seconds", C.green],
  ]);
  pane(b.x, b.y + 246, b.w, b.h - 246, "channel   agents talking", CHANNEL, C.accent);
  side(X, Y, [
    ["Inter-service talk", "Subagents never edit each\nother's repo. They agree on\nthe boundary instead.", C.accent],
    ["The contract", "API shape, events, or a schema.\ntest-agent checks both sides\nagainst it.", C.stroke],
    ["Two services", "Same thing for orders-svc and\ninventory-svc, with an event\nlike stock.reserved.", C.amber],
  ]);
});

// 7. You control the graph
frame("7 You control the space", ...at(7), FW, FH, (X, Y) => {
  header(X, Y, "You steer the graph to fit the work", "Pause, add, remove, or talk to any agent. Anything that widens the space comes back to the plan gate.");
  const b = terminal(X, Y, { tabs: withRate("dev", C.green), current: "rate limit", status: "5 agents   1 writing   contract agreed   $0.74", prompt: "be-agent > use seconds, not a date" });
  pane(b.x, b.y, 560, 260, "graph", tree(["writing", "paused", "watching", "idle", "waiting on you"], 1), C.violet);
  pane(b.x + 576, b.y, b.w - 576, 260, "plan gate: this widens the space", [
    ["+ docs-agent   docs-site", C.amber],
    ["  own worktree in docs-site", C.stroke],
    ["  joins the channel and the contract", C.stroke],
    ["", null],
    ["enter approve   esc cancel", C.dim],
  ], C.amber);
  pane(b.x, b.y + 276, b.w, b.h - 276, "channel", [
    ["orch                contract v2 agreed by both. fe-agent unblocked", C.accent],
    ["orch                fe-agent paused by you. its worktree is kept", C.accent],
    ["orch                docs-agent requested. it joins after you approve", C.accent],
    ["you → be-agent      use seconds, not a date", C.violet],
    ["be-agent → you      got it. telling orch, since it touches the contract", C.amber],
  ], C.accent);
  side(X, Y, [
    ["Your controls", "p pause or resume an agent\na add an agent or a repo\nx remove a helper\nt talk to any agent", C.violet],
    ["Bigger moves", "split a writer into its own space\nmerge two spaces into one\nbring a living space into this one", C.stroke],
    ["One rule", "Adding agents, repos, or budget\nreturns to the plan gate.", C.amber],
  ]);
});

// Agentic loops on a shared space
frame("Agentic loops on a shared space", ...at(), FW, FH, (X, Y) => {
  header(X, Y, "Agents loop on their own around a shared space",
    "Each step is a different agent using a different plugin. They meet in the shared space, not in each other's repos.");
  node(X + 60, Y + 120, 420, 110, "parent agent", "watches the loop and reshapes panes as it\nmoves. stops it at the budget or the gate.", C.violet);

  rect(X + 620, Y + 380, 360, 170, { stroke: C.stroke, strokeWidth: 2 });
  text(X + 640, Y + 396, "shared space", { size: 20 });
  ["contract v2", "findings", "notes, artifacts"].forEach((t, i) => text(X + 640, Y + 440 + i * 30, t, { size: 15, mono: true, color: C.dim }));

  node(X + 600, Y + 150, 400, 96, "1 write   be-agent, fe-agent", "terminal plugin. each in its own worktree", C.amber);
  node(X + 1120, Y + 420, 380, 96, "2 test   test-agent", "terminal plugin. both repos", C.green);
  node(X + 600, Y + 700, 400, 96, "3 probe   test-agent", "api client plugin. POST /checkout x11", C.accent);
  node(X + 120, Y + 420, 380, 96, "4 review   reviewer", "reads diffs and findings", C.violet);
  arrow(X + 1004, Y + 200, X + 1300, Y + 414, { stroke: C.dim });
  arrow(X + 1300, Y + 520, X + 1004, Y + 745, { stroke: C.dim });
  arrow(X + 596, Y + 745, X + 310, Y + 520, { stroke: C.dim });
  arrow(X + 310, Y + 414, X + 596, Y + 210, { stroke: C.dim });
  [[800, 250, 800, 376], [1116, 468, 984, 468], [800, 696, 800, 554], [504, 468, 616, 468]].forEach(([a, b, c, d]) =>
    arrow(X + a, Y + b, X + c, Y + d, { stroke: C.faint, dashed: true, head: false }));
  note(X + 90, Y + 290, "a failure goes back to write,\nwith the finding attached", C.dim);

  rect(X + 1100, Y + 760, 440, 190, { stroke: C.faint });
  text(X + 1120, Y + 776, "how independent   o to change", { size: 17, color: C.amber });
  [
    ["ask each step", "you approve every step"],
    ["within budget", "runs, asks only to widen"],
    ["until checks pass", "runs, then hands you a review"],
  ].forEach(([k, v], i) => {
    text(X + 1120, Y + 816 + i * 40, k, { size: 14, mono: true, color: C.stroke });
    text(X + 1290, Y + 816 + i * 40, v, { size: 14, color: C.dim });
  });
  note(X + 60, Y + 860, "A space can run more than one loop at once, for example\na build loop and a docs loop. They meet in the shared space.", C.stroke);
});

// Parent mounts an API client
frame("Parent mounts an API client", ...at(), FW, FH, (X, Y) => {
  header(X, Y, "The task needs an API client, so the parent mounts one",
    "It's a plugin pane, like the browser. The agents call it too, which is how the probe step runs without you.");
  const b = terminal(X, Y, {
    tabs: withRate("dev", C.green), current: "rate limit",
    status: "4 agents   loop round 2   until checks pass   $1.10 / $5   m plugins   o autonomy",
  });
  pane(b.x, b.y, 380, b.h, "agents", [
    ...tree(["writing", "writing", "probing", "idle"]),
    ["", null],
    ["loop  write → test → [probe] → review", C.amber],
    ["round 2   until checks pass", C.dim],
    ["shared: contract v2, findings", C.dim],
  ]);
  const rx = b.x + 396, rw = b.w - 396, half = (rw - 16) / 2;
  pane(rx, b.y, half, 300, "api client   plugin", [
    ["POST /checkout   x11, then retry", C.stroke],
    ["← 429  Retry-After: 12", C.amber],
    ["← 200  charged once", C.green],
    ["", null],
    ["history  3 requests", C.dim],
    ["scope    local, staging", C.dim],
  ], C.violet);
  pane(rx + half + 16, b.y, half, 300, "browser   plugin", [
    [":5173/checkout", C.dim],
    ["429 shows 'try again in 12s'", C.stroke],
  ], C.violet);
  pane(rx, b.y + 316, rw, b.h - 316, "channel", [
    ["parent              mounted api client: this task changes an API", C.violet],
    ["test-agent → api    replaying POST /checkout x11", C.stroke],
    ["api → shared        finding: retry after 429 charged once", C.green],
    ["orch                probe passed. moving to review", C.accent],
  ], C.accent);
  side(X, Y, [
    ["Shaped by the task", "The parent saw an API change\nand mounted a request pane.\nYou can unmount it with m.", C.violet],
    ["Agents use it too", "test-agent calls http.replay.\nThe result lands in the shared\nspace as a finding.", C.green],
  ]);
});

// 8. Reshaped
frame("8 Reshaped", ...at(8), FW, FH, (X, Y) => {
  header(X, Y, "The task changed: the same space reshapes", "A test failure turns building into debugging. Same tab, same agents. Adding attempts comes back to you first.");
  const b = terminal(X, Y, { tabs: withRate("dev → debug", C.red), current: "rate limit", status: "reshaping   inbox 1   $0.81   enter approve   esc stay in dev" });
  pane(b.x, b.y, b.w, 200, "event", [
    ["test-agent    FAIL limit.spec.ts   a retry after 429 is charged twice", C.red],
    ["orch          this is a bug in the limiter, not more building", C.accent],
    ["parent agent  will bring logs and attempts forward", C.violet],
  ], C.red);
  pane(b.x, b.y + 216, b.w, b.h - 216, "plan gate, again", [
    ["keep    be-agent becomes attempt 1. fe-agent and helpers stay.", C.stroke],
    ["add     attempt 2 and attempt 3: forks of be-agent, each in its own worktree", C.amber],
    ["", null],
    ["fe-agent keeps its place. the contract does not change.", C.dim],
  ], C.amber);
  side(X, Y, [
    ["Not a new page", "The tab tag goes from dev to\ndebug. The graph is the same.", C.red],
    ["Why it asks", "Attempts widen the space, so\nthe orchestrator brings it back\nto you before they run.", C.amber],
  ]);
});

// 9. Debug
frame("9 Shaped as debug", ...at(9), FW, FH, (X, Y) => {
  header(X, Y, "Now a debug shape: logs, repro, attempts", "Attempts are forks of the backend writer. You compare outcomes, not branches.");
  const b = terminal(X, Y, { tabs: withRate("debug", C.red), current: "rate limit", status: "6 agents   3 attempts   $1.40   1 review attempt 1   v building again   g graph" });
  pane(b.x, b.y, 380, 260, "logs   payments", [
    ["12:40 POST /checkout 429", C.dim],
    ["12:40 retry after 12s", C.dim],
    ["12:41 charge cart 8812", C.red],
    ["12:41 charge cart 8812", C.red],
  ]);
  pane(b.x, b.y + 276, 380, b.h - 276, "repro", [
    ["before    charged twice", C.red],
    ["attempt 1 charged once", C.green],
    ["attempt 2 charged once", C.green],
    ["attempt 3 running", C.dim],
  ]);
  pane(b.x + 396, b.y, b.w - 396, b.h, "attempts   forks of be-agent", [
    ["1  idempotency key on the retry     42/42   +24 -2    ready", C.green],
    ["2  limiter skips paid carts         40/42   +61 -9    2 failing", C.amber],
    ["3  queue retries behind the lock    step 3 of 5", C.dim],
    ["", null],
    ["fe-agent   checkout-web   idle, contract v2 unchanged", C.green],
  ]);
  side(X, Y, [["Same space", "Its name, repos, and graph\nare the same. Only the\nshape changed.", C.red]]);
});

// 10. Review
frame("10 Review, ship, dissolve", ...at(10), FW, FH, (X, Y) => {
  header(X, Y, "Review happens inside the space, then it dissolves", "One review across both repos. Shipping opens both PRs and removes every worktree.");
  const b = terminal(X, Y, { tabs: withRate("review", C.violet), current: "rate limit", status: "reviewing attempt 1   s merge and ship   b send back" });
  pane(b.x, b.y, 300, b.h, "what lands", [
    ["payments", C.amber],
    [" src/limit.ts   +18", C.stroke],
    [" src/retry.ts   +6 -2", C.stroke],
    ["checkout-web", C.green],
    [" Checkout.tsx   +9", C.stroke],
  ]);
  pane(b.x + 316, b.y, 440, b.h, "src/retry.ts", [
    ["+ const key = idemKey(cart)", C.green],
    ["- await charge(cart)", C.red],
    ["+ await charge(cart, { key })", C.green],
    ["", null],
    ["a retry after 429 reuses the first charge.", C.stroke],
  ]);
  pane(b.x + 772, b.y, b.w - 772, b.h, "checks", [
    ["tests 42/42", C.green],
    ["contract v2 both sides", C.green],
    ["reviewer ok", C.green],
    ["no conflicts", C.green],
  ]);
  side(X, Y, [
    ["Review is a shape", "The tab tag reads review.\nIt is this space, not a\nseparate page.", C.violet],
    ["After s", "PRs open in both repos, the\nticket is linked, the worktrees\nare removed, the tab goes away.", C.green],
  ]);
});

// 11. Still living
frame("11 Still living", ...at(11), FW, FH, (X, Y) => {
  header(X, Y, "After it ships, the rest keeps living", "The parent agent is never finished. Two spaces are still alive, and a new task can arrive at any time.");
  const b = terminal(X, Y, { tabs: TABS, current: "home", status: "2 spaces alive   inbox 2   $5.34 today   tab next space   n new task" });
  pane(b.x, b.y, b.w, 200, "shipped", [
    ["  rate limit   PRs: payments#212, checkout-web#88", C.violet],
    ["  its 5 agents finished and its worktrees were removed", C.dim],
  ], C.violet);
  pane(b.x, b.y + 216, b.w, b.h - 216, "living spaces", [
    ["  order sync     dev     4 agents   orders-svc ↔ inventory-svc", C.green],
    ["  checkout 500   debug   4 agents   payments ↔ checkout-web", C.red],
  ]);
  side(X, Y, [["No end state", "Home again, because it is just\nanother tab. The parent keeps\nmaking and reshaping spaces.", C.stroke]]);
});

process.stdout.write(scene("pixel-workspace-rs/design/gen.mjs"));
