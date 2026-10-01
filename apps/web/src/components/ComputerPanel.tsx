import { useEffect, useRef, useState } from "react";
import { api, attempt, liveFrameUrl } from "../lib/api";
import { useLive } from "../lib/hooks";
import type { BotWithStatus } from "../lib/appdata";
import type { LiveInfo } from "../lib/types";
import { useToast } from "../lib/toast";
import { Lazy, lazyPage } from "../lib/lazy";
import { RunList } from "../pages/Activity";
import { Mascot } from "./Mascot";
import { Button, cx, inputCls } from "./ui";

const Files = lazyPage(() => import("../pages/Files").then((m) => ({ default: m.Files })));

type Tab = "browser" | "files" | "activity";

/** Live info about a bot's browser; `ver` changes whenever a new frame lands. */
export function useLiveFrame(botId: string) {
  const [info, setInfo] = useState<LiveInfo | null>(null);
  const [ver, setVer] = useState(0);
  const load = () => {
    api.get<LiveInfo>(`/api/bots/${botId}/live`).then(
      (i) => { setInfo(i); setVer(Date.now()); },
      () => { /* 404: no session yet */ },
    );
  };
  useEffect(() => { setInfo(null); load(); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [botId]);
  useLive(["live_frames"], load, { bot: botId });
  return { info, ver };
}

export function ComputerPanel({ bot, info, ver, onClose }: { bot: BotWithStatus; info: LiveInfo | null; ver: number; onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("browser");
  return (
    <aside className="h-full flex flex-col bg-surface border-l border-line min-h-0" aria-label="Computer">
      <div className="flex items-center gap-1 px-2 border-b border-line shrink-0">
        {(["browser", "files", "activity"] as Tab[]).map((t) => (
          <button key={t} onClick={() => setTab(t)}
            className={cx("px-3 min-h-11 text-sm font-medium border-b-2 -mb-px cursor-pointer", tab === t ? "border-accent text-ink" : "border-transparent text-muted hover:text-ink")}>
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
        <Button tone="ghost" className="ml-auto" onClick={onClose} aria-label="Close computer panel">✕</Button>
      </div>
      <div className="flex-1 min-h-0 overflow-auto">
        {tab === "browser" && <BrowserTab bot={bot} info={info} ver={ver} />}
        {tab === "files" && <div className="p-3"><Lazy compact><Files bot={bot} /></Lazy></div>}
        {tab === "activity" && <div className="p-3"><RunList bot={bot} /></div>}
      </div>
    </aside>
  );
}

function BrowserTab({ bot, info, ver }: { bot: BotWithStatus; info: LiveInfo | null; ver: number }) {
  const toast = useToast();
  const [url, setUrl] = useState("");
  const [control, setControl] = useState(false);
  const [text, setText] = useState("");
  const img = useRef<HTMLImageElement>(null);
  useEffect(() => { if (info?.url) setUrl(info.url); }, [info?.url]);

  // swap frames without a blank: decode the next one off-DOM, then change src
  const target = liveFrameUrl(bot.id, ver);
  const [shown, setShown] = useState(target);
  const latest = useRef(target);
  latest.current = target;
  useEffect(() => {
    if (target === shown) return;
    let dead = false;
    const next = new Image();
    next.src = target;
    const swap = () => { if (!dead && latest.current === target) setShown(target); };
    next.decode().then(swap, swap);
    return () => { dead = true; };
  }, [target, shown]);

  const send = async (body: Record<string, unknown>) => {
    const e = await attempt(() => api.post(`/api/bots/${bot.id}/live/input`, body));
    if (e) toast(e, "bad");
  };

  if (!info) {
    return (
      <div className="p-8 text-center">
        <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} size={110} state="idle" className="mb-3" />
        <p className="font-medium">No browser session yet</p>
        <p className="text-sm text-muted">When {bot.name} opens a web page, you can watch it here and take over.</p>
      </div>
    );
  }

  function frameCoords(e: { clientX: number; clientY: number }) {
    const el = img.current!;
    const rect = el.getBoundingClientRect();
    const w = info?.width || el.naturalWidth, h = info?.height || el.naturalHeight;
    return { x: Math.round(((e.clientX - rect.left) / rect.width) * w), y: Math.round(((e.clientY - rect.top) / rect.height) * h) };
  }

  return (
    <div className="p-3 space-y-3">
      <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); if (url.trim()) void send({ type: "navigate", url: url.trim() }); }}>
        <input className={inputCls + " mono !min-h-9 !py-1"} value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://" aria-label="Address" />
        <Button type="submit" disabled={!url.trim()}>Go</Button>
      </form>
      <div className={cx("relative rounded-[12px] overflow-hidden border bg-sunken", control ? "border-accent ring-2 ring-accent/30" : "border-line")}>
        <img
          ref={img}
          src={shown}
          alt={info.title || "Live browser view"}
          draggable={false}
          tabIndex={control ? 0 : -1}
          className={cx("w-full h-auto block select-none outline-none", control && "cursor-crosshair")}
          onClick={(e) => { if (control) void send({ type: "click", ...frameCoords(e) }); }}
          onWheel={(e) => { if (control) void send({ type: "scroll", dy: Math.round(e.deltaY) }); }}
          onKeyDown={(e) => {
            if (!control) return;
            if (e.key.length === 1 && !e.ctrlKey && !e.metaKey) { e.preventDefault(); void send({ type: "type", text: e.key }); }
            else if (["Enter", "Backspace", "Tab", "Escape", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.key)) { e.preventDefault(); void send({ type: "key", key: e.key }); }
          }}
        />
        {control && <span className="absolute top-2 left-2 rounded-full bg-accent text-accent-ink text-xs px-2 py-0.5">You're in control</span>}
      </div>
      <p className="text-xs text-muted truncate">{info.title}</p>
      <div className="flex items-center gap-2 flex-wrap">
        <Button tone={control ? "primary" : "default"} onClick={() => setControl((c) => !c)}>{control ? "Return control" : "Take over"}</Button>
        {!control && <span className="text-xs text-muted">{bot.name} is driving.</span>}
      </div>
      {control && (
        <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); if (text) { void send({ type: "type", text }); setText(""); } }}>
          <input className={inputCls + " !min-h-9 !py-1"} value={text} onChange={(e) => setText(e.target.value)} placeholder="Type into the page" aria-label="Text to type" />
          <Button type="submit" disabled={!text}>Type</Button>
          <Button type="button" onClick={() => void send({ type: "key", key: "Enter" })}>Enter</Button>
        </form>
      )}
    </div>
  );
}
