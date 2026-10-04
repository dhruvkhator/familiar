import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { BotEngine } from "../lib/types";
import { Field, inputCls } from "./ui";

export const CLAUDE_ALIASES = ["sonnet", "opus", "haiku", "fable"];
export const CODEX_SUGGESTIONS = ["gpt-5-codex", "gpt-5"];

interface ClaudeModel { id: string; label: string; alias: boolean; available: boolean }
interface ModelCatalog {
  claude: { plan: string | null; models: ClaudeModel[] };
  codex: { models: { id: string; label: string }[] };
}

export function defaultModel(engine: BotEngine): string {
  return engine === "codex" ? "gpt-5-codex" : "sonnet";
}

function aliasLabel(a: string) { return a.charAt(0).toUpperCase() + a.slice(1) + " · latest"; }

let cached: ModelCatalog | null = null;
function useModels(): ModelCatalog | null | undefined {
  const [m, setM] = useState<ModelCatalog | null | undefined>(cached ?? undefined);
  useEffect(() => {
    let live = true;
    api.get<ModelCatalog>("/api/models").then((r) => { cached = r; if (live) setM(r); }).catch(() => { if (live) setM(null); });
    return () => { live = false; };
  }, []);
  return m;
}

/** Engine picker plus the model control that fits it. */
export function EngineFields({ engine, model, onChange }: { engine: BotEngine; model: string; onChange: (engine: BotEngine, model: string) => void }) {
  const cat = useModels();
  const claudeList = cat?.claude?.models ?? [];
  const plan = cat?.claude?.plan ?? null;
  const ok = claudeList.filter((x) => x.available);
  const off = claudeList.filter((x) => !x.available);
  const known = claudeList.some((x) => x.id === model) || CLAUDE_ALIASES.includes(model);
  const codexList = cat?.codex?.models ?? [];
  const label = (x: ClaudeModel) => (x.alias && x.label.toLowerCase() === x.id.toLowerCase() ? aliasLabel(x.id) : x.label);

  return (
    <>
      <Field label="Engine" hint={engine === "codex" ? "Runs on your Codex CLI sign-in." : "Runs on your Claude Code sign-in."}>
        <select className={inputCls} value={engine} onChange={(e) => {
          const next = e.target.value as BotEngine;
          onChange(next, next === engine ? model : defaultModel(next));
        }}>
          <option value="claude">Claude</option>
          <option value="codex">Codex</option>
        </select>
      </Field>
      {engine === "claude" ? (
        <Field label="Model" hint={claudeList.length === 0 ? (cat === undefined || plan ? "Checking your plan…" : "Sign in to Claude to see your models") : plan ? `Claude ${plan} plan` : undefined}>
          <select className={inputCls} value={model} onChange={(e) => onChange(engine, e.target.value)}>
            {claudeList.length === 0 ? (
              <>
                {!CLAUDE_ALIASES.includes(model) && <option value={model}>{model}</option>}
                {CLAUDE_ALIASES.map((a) => <option key={a} value={a}>{aliasLabel(a)}</option>)}
              </>
            ) : (
              <>
                {!known && <option value={model}>{model}</option>}
                {ok.map((x) => <option key={x.id} value={x.id}>{label(x)}</option>)}
                {off.length > 0 && (
                  <optgroup label="Not on your plan">
                    {off.map((x) => <option key={x.id} value={x.id} disabled>{label(x)}</option>)}
                  </optgroup>
                )}
              </>
            )}
          </select>
        </Field>
      ) : (
        <Field label="Model" hint="Pick one, or type any model your Codex CLI accepts.">
          <input className={inputCls + " mono"} list="codex-models" value={model} onChange={(e) => onChange(engine, e.target.value)} placeholder="gpt-5-codex" />
          <datalist id="codex-models">
            {(codexList.length ? codexList : CODEX_SUGGESTIONS.map((id) => ({ id, label: id }))).map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
          </datalist>
        </Field>
      )}
    </>
  );
}
