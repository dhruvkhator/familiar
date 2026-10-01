import { useEffect, type ButtonHTMLAttributes, type ReactNode } from "react";
import type { RunStatus } from "../lib/types";

export function cx(...a: (string | false | null | undefined)[]) {
  return a.filter(Boolean).join(" ");
}

type BtnProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  tone?: "default" | "primary" | "danger" | "ghost";
  big?: boolean;
};
export function Button({ tone = "default", big, className, ...p }: BtnProps) {
  const tones = {
    default: "bg-surface border-line text-ink hover:bg-sunken",
    primary: "bg-accent border-accent text-accent-ink hover:brightness-110",
    danger: "bg-surface border-bad text-bad hover:bg-bad-soft",
    ghost: "bg-transparent border-transparent text-muted hover:text-ink hover:bg-sunken",
  } as const;
  return (
    <button
      {...p}
      className={cx(
        "inline-flex items-center justify-center gap-2 rounded-[10px] border font-medium select-none",
        "disabled:opacity-45 disabled:pointer-events-none transition-colors cursor-pointer",
        big ? "min-h-12 px-5 text-base" : "min-h-9 px-3 text-sm",
        tones[tone],
        className,
      )}
    />
  );
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block">
      <span className="block text-sm font-medium mb-1">{label}</span>
      {children}
      {hint && <span className="block text-xs text-muted mt-1">{hint}</span>}
    </label>
  );
}

export const inputCls =
  "w-full rounded-[10px] border border-line bg-surface px-3 py-2 text-[15px] placeholder:text-muted/70 min-h-10";

export function Chip({ tone = "muted", children }: { tone?: "muted" | "accent" | "ok" | "warn" | "bad"; children: ReactNode }) {
  const t = {
    muted: "bg-sunken text-muted",
    accent: "bg-accent-soft text-accent",
    ok: "bg-accent-soft text-ok",
    warn: "bg-warn-soft text-warn",
    bad: "bg-bad-soft text-bad",
  }[tone];
  return <span className={cx("inline-flex items-center rounded px-1.5 py-0.5 text-xs font-medium whitespace-nowrap", t)}>{children}</span>;
}

export function runTone(s: RunStatus): "muted" | "accent" | "ok" | "warn" | "bad" {
  switch (s) {
    case "running": return "accent";
    case "queued": return "muted";
    case "waiting_approval": return "warn";
    case "succeeded": return "ok";
    case "failed": return "bad";
    default: return "muted";
  }
}
export function RunChip({ status }: { status: RunStatus }) {
  return <Chip tone={runTone(status)}>{status.replace("_", " ")}</Chip>;
}

export function Led({ status }: { status: "idle" | "running" | "paused" | "online" | "offline" }) {
  const c = {
    idle: "border border-muted/60",
    running: "bg-accent led-run",
    paused: "bg-warn",
    online: "bg-ok",
    offline: "bg-bad",
  }[status];
  return <span aria-hidden className={cx("inline-block size-2.5 rounded-full shrink-0", c)} />;
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="rounded-[14px] border border-dashed border-line px-5 py-8 text-center">
      <p className="font-medium">{title}</p>
      {children && <p className="text-sm text-muted mt-1">{children}</p>}
    </div>
  );
}

export function ErrorNote({ error }: { error: string | null | undefined }) {
  if (!error) return null;
  return <p role="alert" className="rounded-md bg-bad-soft text-bad text-sm px-3 py-2 break-words">{error}</p>;
}

export function Spinner({ label = "Loading" }: { label?: string }) {
  return (
    <div className="space-y-2 py-3" role="status" aria-label={label}>
      <div className="skeleton h-12" /><div className="skeleton h-12 opacity-70" /><div className="skeleton h-12 opacity-40" />
    </div>
  );
}

export function Dialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    const f = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", f);
    return () => window.removeEventListener("keydown", f);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-end sm:items-center justify-center bg-black/45 p-0 sm:p-4" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onMouseDown={(e) => e.stopPropagation()}
        className="settle w-full sm:max-w-lg max-h-[92dvh] overflow-auto rounded-t-xl sm:rounded-[18px] border border-line bg-surface p-5 safe-b"
      >
        <div className="flex items-center justify-between mb-4">
          <h2 className="text-lg font-semibold">{title}</h2>
          <Button tone="ghost" onClick={onClose} aria-label="Close">✕</Button>
        </div>
        {children}
      </div>
    </div>
  );
}

const paths: Record<string, string> = {
  bots: "M5 8h14v9H5zM12 4v4M9 12h.01M15 12h.01",
  inbox: "M4 13l2-8h12l2 8v5H4zM4 13h5l1 2h4l1-2h5",
  shield: "M12 3l7 3v5c0 5-3 8-7 10-4-2-7-5-7-10V6z",
  sun: "M12 8a4 4 0 100 8 4 4 0 000-8zM12 2v2M12 20v2M4 12H2M22 12h-2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5-1.5M17.5 6.5L19 5",
  moon: "M20 14A8 8 0 019.5 4 8 8 0 1020 14z",
  plus: "M12 5v14M5 12h14",
  back: "M15 5l-7 7 7 7",
  plug: "M9 3v5M15 3v5M6 8h12v3a6 6 0 01-12 0zM12 17v4",
  out: "M9 4H5v16h4M16 8l4 4-4 4M20 12H9",
  cog: "M4 7h9M17 7h3M4 17h3M11 17h9M15 5v4M9 15v4",
};
export function Icon({ name, className }: { name: keyof typeof paths | string; className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={cx("size-5 shrink-0", className)} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d={paths[name] ?? ""} />
    </svg>
  );
}

export function Badge({ n }: { n: number }) {
  if (!n) return null;
  return <span className="ml-auto min-w-5 h-5 px-1.5 rounded-full bg-warn text-[11px] leading-5 font-semibold text-center text-black tnum">{n}</span>;
}
