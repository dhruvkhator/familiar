import { cx } from "./ui";

export type MascotState = "working" | "needs-you" | "done" | "paused" | "idle";
export type Accessory = "none" | "hat" | "glasses" | "headphones" | "bow" | "antenna" | "crown";

/** Stored as `bots.avatar` (jsonb). Every field is optional; missing ones fall back to the id-derived default. */
export interface Avatar { shape: number; color: string; eyes: number; mouth: number; accessory: Accessory }

export const PALETTE = ["#7285d5", "#e58fa4", "#4fb98a", "#eda84b", "#a283d8", "#4fa9cf"] as const;
export const ACCESSORIES: Accessory[] = ["none", "hat", "glasses", "headphones", "bow", "antenna", "crown"];
export const SHAPE_COUNT = 5, EYE_COUNT = 5, MOUTH_COUNT = 4;

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  return Math.abs(h);
}

export function defaultAvatar(id: string): Avatar {
  const h = hash(id);
  return { shape: h % SHAPE_COUNT, color: PALETTE[(h >> 3) % PALETTE.length], eyes: 0, mouth: 0, accessory: "none" };
}
export function randomAvatar(): Avatar {
  const pick = <T,>(a: readonly T[]) => a[Math.floor(Math.random() * a.length)];
  return {
    shape: Math.floor(Math.random() * SHAPE_COUNT),
    color: Math.random() < 0.7 ? pick(PALETTE) : hueToHex(Math.floor(Math.random() * 360)),
    eyes: Math.floor(Math.random() * EYE_COUNT),
    mouth: Math.floor(Math.random() * MOUTH_COUNT),
    accessory: pick(ACCESSORIES),
  };
}
export function hueToHex(h: number): string {
  const s = 0.5, l = 0.6;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    const c = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(255 * c).toString(16).padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

/** Merge a possibly partial / invalid stored avatar over the default. */
export function resolveAvatar(id: string, a?: Partial<Avatar> | null): Avatar {
  const d = defaultAvatar(id);
  if (!a || typeof a !== "object") return d;
  const num = (v: unknown, max: number, dflt: number) => (typeof v === "number" && v >= 0 && v < max ? Math.floor(v) : dflt);
  return {
    shape: num(a.shape, SHAPE_COUNT, d.shape),
    color: typeof a.color === "string" && /^#[0-9a-f]{6}$/i.test(a.color) ? a.color : d.color,
    eyes: num(a.eyes, EYE_COUNT, d.eyes),
    mouth: num(a.mouth, MOUTH_COUNT, d.mouth),
    accessory: ACCESSORIES.includes(a.accessory as Accessory) ? (a.accessory as Accessory) : d.accessory,
  };
}

// body outlines in a 100x100 box; top = highest y, dy = vertical shift for the face
const SHAPES: { d: string; top: number; dy: number }[] = [
  { d: "M50 10c22 0 38 15 38 38 0 24-14 42-38 42S12 72 12 48C12 25 28 10 50 10z", top: 10, dy: 0 },
  { d: "M50 6c20 0 32 18 32 42 0 28-14 44-32 44S18 76 18 48C18 24 30 6 50 6z", top: 6, dy: -2 },
  { d: "M30 14h40c12 0 20 8 20 20v34c0 12-8 20-20 20H30C18 88 10 80 10 68V34c0-12 8-20 20-20z", top: 14, dy: 0 },
  { d: "M50 20c26 0 42 12 42 34 0 20-16 34-42 34S8 74 8 54C8 32 24 20 50 20z", top: 20, dy: 7 },
  { d: "M14 52C14 28 30 10 50 10s36 18 36 42v38l-12-8-12 8-12-8-12 8-12-8z", top: 10, dy: 0 },
];

const INK = "#2b2d3a";

function Eyes({ style, state }: { style: number; state: MascotState }) {
  if (state === "paused") {
    return <g stroke={INK} strokeWidth="4" strokeLinecap="round" fill="none"><path d="M30 47q7 6 14 0" /><path d="M56 47q7 6 14 0" /></g>;
  }
  const wide = state === "needs-you";
  const look = state === "working" ? 2 : 0;
  const eye = (cx0: number) => {
    switch (wide ? 0 : style) {
      case 1: // big
        return <><ellipse cx={cx0} cy="46" rx="9" ry="11" fill="#fff" /><circle cx={cx0 + look} cy="47" r="5" fill={INK} /><circle cx={cx0 + look + 1.6} cy="45" r="1.6" fill="#fff" /></>;
      case 2: // dots
        return <circle cx={cx0 + look / 2} cy="47" r="4.6" fill={INK} />;
      case 3: // happy arcs
        return <path d={`M${cx0 - 7} 49q7 -9 14 0`} stroke={INK} strokeWidth="4" strokeLinecap="round" fill="none" />;
      case 4: // sleepy lids
        return <><ellipse cx={cx0} cy="47" rx="7" ry="7" fill="#fff" /><circle cx={cx0 + look} cy="48" r="3.8" fill={INK} /><path d={`M${cx0 - 8} 44h16`} stroke={INK} strokeWidth="3.5" strokeLinecap="round" /></>;
      default:
        return <><ellipse cx={cx0} cy="46" rx={wide ? 8.5 : 7} ry={wide ? 10 : 8.5} fill="#fff" /><circle cx={cx0 + look} cy="47" r="4" fill={INK} /></>;
    }
  };
  return <g className="mascot-eyes">{eye(37)}{eye(63)}</g>;
}

function Mouth({ style, state }: { style: number; state: MascotState }) {
  if (state === "done") return <path d="M40 64q10 10 20 0" stroke={INK} strokeWidth="3.5" strokeLinecap="round" fill="none" />;
  if (state === "needs-you") return <ellipse cx="50" cy="68" rx="4.5" ry="5.5" fill={INK} />;
  switch (style) {
    case 1: return <path d="M43 67h14" stroke={INK} strokeWidth="3.2" strokeLinecap="round" />;
    case 2: return <path d="M40 63h20q0 12-10 12T40 63z" fill={INK} />;
    case 3: return <path d="M40 65q5 6 10 0q5 6 10 0" stroke={INK} strokeWidth="3" strokeLinecap="round" fill="none" />;
    default: return <path d="M43 66q7 4 14 0" stroke={INK} strokeWidth="3" strokeLinecap="round" fill="none" opacity="0.85" />;
  }
}

function Accessory({ kind, top }: { kind: Accessory; top: number }) {
  const t = top - 10; // shift relative to the round body
  switch (kind) {
    case "hat":
      return <g transform={`translate(0 ${t})`}><path d="M31 24c0-17 8-24 19-24s19 7 19 24z" fill="#3a3f55" /><rect x="25" y="22" width="50" height="6" rx="3" fill="#2b2d3a" /><rect x="31" y="16" width="38" height="3.5" fill="#e58fa4" /></g>;
    case "glasses":
      return <g fill="none" stroke={INK} strokeWidth="3"><circle cx="37" cy="46" r="11.5" fill="#fff" fillOpacity="0.25" /><circle cx="63" cy="46" r="11.5" fill="#fff" fillOpacity="0.25" /><path d="M48.5 46h3" /></g>;
    case "headphones":
      return <g transform={`translate(0 ${t / 2})`}><path d="M15 54C15 26 30 12 50 12s35 14 35 42" fill="none" stroke="#3a3f55" strokeWidth="6" strokeLinecap="round" /><rect x="8" y="46" width="12" height="22" rx="6" fill="#3a3f55" /><rect x="80" y="46" width="12" height="22" rx="6" fill="#3a3f55" /></g>;
    case "bow":
      return <g transform={`translate(0 ${t})`}><path d="M68 18l16-9v18z M68 18l-16-9v18z" fill="#e0527a" /><circle cx="68" cy="18" r="4" fill="#b83a60" /></g>;
    case "antenna":
      return <g transform={`translate(0 ${t})`}><path d="M50 12V-2" stroke="#3a3f55" strokeWidth="3" strokeLinecap="round" /><circle cx="50" cy="-4" r="5" fill="#f26b5b" /></g>;
    case "crown":
      return <g transform={`translate(0 ${t})`}><path d="M32 22l3-16 9 8 6-14 6 14 9-8 3 16z" fill="#f2c14e" stroke="#c99a2a" strokeWidth="1.5" strokeLinejoin="round" /></g>;
    default:
      return null;
  }
}

export function Mascot({ id, name, avatar, state = "idle", size = 54, className }: {
  id: string; name: string; avatar?: Partial<Avatar> | null; state?: MascotState; size?: number; className?: string;
}) {
  const a = resolveAvatar(id, avatar);
  const shape = SHAPES[a.shape];
  const cls = { working: "mascot-working", "needs-you": "mascot-needs", done: "mascot-done", paused: "mascot-paused", idle: "" }[state];
  const gid = `mg-${id.replace(/[^a-z0-9]/gi, "").slice(0, 10)}-${size}-${a.shape}`;
  return (
    <span className={cx("mascot", cls, className)} style={{ width: size, height: size }} role="img" aria-label={`${name} is ${state.replace("-", " ")}`}>
      <svg viewBox="0 -10 100 110" width={size} height={size * 1.1} style={{ marginTop: -size * 0.1 / 2, overflow: "visible" }} aria-hidden>
        <defs>
          <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="#fff" stopOpacity="0.28" />
            <stop offset="0.55" stopColor="#fff" stopOpacity="0" />
            <stop offset="1" stopColor="#000" stopOpacity="0.14" />
          </linearGradient>
        </defs>
        {state === "needs-you" && <circle cx="50" cy="52" r="52" fill="none" stroke="var(--warn)" strokeWidth="3.5" strokeDasharray="6 6" opacity="0.85" />}
        <path d={shape.d} fill={a.color} />
        <path d={shape.d} fill={`url(#${gid})`} />
        <g transform={`translate(0 ${shape.dy})`}>
          <Eyes style={a.eyes} state={state} />
          <Mouth style={a.mouth} state={state} />
          <circle cx="25" cy="60" r="5" fill="#fff" opacity="0.22" />
          <circle cx="75" cy="60" r="5" fill="#fff" opacity="0.22" />
          {a.accessory === "glasses" && <Accessory kind="glasses" top={shape.top} />}
        </g>
        {a.accessory !== "glasses" && <Accessory kind={a.accessory} top={shape.top} />}
      </svg>
    </span>
  );
}

export const STATE_LABEL: Record<MascotState, string> = {
  working: "Working", "needs-you": "Needs you", done: "Just finished", paused: "Paused", idle: "Idle",
};

/** Build the Mascot props for a bot-like object. */
export function botMascot(b: { id: string; name: string; avatar?: Partial<Avatar> | null }) {
  return { id: b.id, name: b.name, avatar: b.avatar };
}
