// Tiny helpers for hand-building Excalidraw scenes. Colors assume the dark #121212 canvas.
export const C = {
  stroke: "#e9ecef",
  dim: "#868e96",
  faint: "#495057",
  accent: "#4dabf7",
  green: "#69db7c",
  amber: "#ffd43b",
  red: "#ff8787",
  violet: "#b197fc",
  ink: "#121212",
};

const elements = [];
let n = 0;
let currentFrame = null;
const id = (p) => `${p}_${++n}`;
const seed = () => Math.floor(Math.random() * 2 ** 31);

function base(type, x, y, w, h, o = {}) {
  return {
    id: id(type),
    type,
    x,
    y,
    width: w,
    height: h,
    angle: 0,
    strokeColor: o.stroke ?? C.stroke,
    backgroundColor: o.bg ?? "transparent",
    fillStyle: "solid",
    strokeWidth: o.strokeWidth ?? 1,
    strokeStyle: o.dashed ? "dashed" : "solid",
    roughness: 1,
    opacity: o.opacity ?? 100,
    groupIds: [],
    frameId: currentFrame,
    roundness: o.round === false ? null : { type: 3 },
    seed: seed(),
    version: 1,
    versionNonce: seed(),
    isDeleted: false,
    boundElements: null,
    updated: Date.now(),
    link: null,
    locked: false,
  };
}

export function text(x, y, str, o = {}) {
  const size = o.size ?? 18;
  const lines = str.split("\n");
  const w = Math.max(...lines.map((l) => l.length)) * size * 0.55;
  const h = lines.length * size * 1.25;
  const el = {
    ...base("text", x, y, w, h, { stroke: o.color ?? C.stroke, round: false, opacity: o.opacity }),
    text: str,
    originalText: str,
    fontSize: size,
    fontFamily: o.mono ? 3 : 5,
    textAlign: o.align ?? "left",
    verticalAlign: "top",
    containerId: null,
    lineHeight: 1.25,
    autoResize: true,
  };
  if (o.align === "center") el.x = x - w / 2;
  elements.push(el);
  return el;
}

export function rect(x, y, w, h, o = {}) {
  elements.push(base("rectangle", x, y, w, h, o));
  if (o.label) {
    const size = o.labelSize ?? 18;
    const lines = o.label.split("\n").length;
    const ty = o.labelTop ? y + 10 : y + h / 2 - (lines * size * 1.25) / 2;
    text(x + w / 2, ty, o.label, { size, align: "center", color: o.labelColor ?? o.stroke });
  }
}

export function ellipse(x, y, w, h, o = {}) {
  elements.push(base("ellipse", x, y, w, h, o));
  if (o.label) {
    const size = o.labelSize ?? 14;
    text(x + w / 2, y + h / 2 - (size * 1.25) / 2, o.label, { size, align: "center", color: o.stroke });
  }
}

export function arrow(x1, y1, x2, y2, o = {}) {
  elements.push({
    ...base("arrow", x1, y1, Math.abs(x2 - x1), Math.abs(y2 - y1), { ...o, stroke: o.stroke ?? C.dim, round: false }),
    points: [
      [0, 0],
      [x2 - x1, y2 - y1],
    ],
    lastCommittedPoint: null,
    startBinding: null,
    endBinding: null,
    startArrowhead: null,
    endArrowhead: o.head === false ? null : "arrow",
  });
}

export const note = (x, y, str, color = C.dim) => text(x, y, str, { size: 15, color });

export const dot = (x, y, color, r = 12) => ellipse(x, y, r, r, { stroke: color, bg: color });

export function chip(x, y, label, color = C.dim, o = {}) {
  const size = o.size ?? 14;
  const w = o.w ?? label.length * size * 0.55 + 26;
  rect(x, y, w, o.h ?? 30, { stroke: color, label, labelSize: size, dashed: o.dashed, bg: o.bg, labelColor: o.labelColor });
  return w;
}

export function button(x, y, label, color, o = {}) {
  return chip(x, y, label, color, {
    h: 36,
    size: 15,
    w: o.w,
    bg: o.primary ? color : undefined,
    labelColor: o.primary ? C.ink : color,
  });
}

export function frame(name, x, y, w, h, draw) {
  const f = { ...base("frame", x, y, w, h, { round: false }), name };
  elements.push(f);
  currentFrame = f.id;
  draw(x, y);
  currentFrame = null;
}

export function scene(source) {
  return JSON.stringify(
    {
      type: "excalidraw",
      version: 2,
      source,
      elements,
      appState: { viewBackgroundColor: C.ink, exportWithDarkMode: false, gridSize: null },
      files: {},
    },
    null,
    2
  );
}
