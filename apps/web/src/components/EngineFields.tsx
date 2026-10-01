import type { BotEngine } from "../lib/types";
import { Field, inputCls } from "./ui";

export const CLAUDE_MODELS = ["sonnet", "opus", "haiku", "fable"];
export const CODEX_SUGGESTIONS = ["gpt-5-codex", "gpt-5"];

export function defaultModel(engine: BotEngine): string {
  return engine === "codex" ? "gpt-5-codex" : "sonnet";
}

/** Engine picker plus the model control that fits it. */
export function EngineFields({ engine, model, onChange }: { engine: BotEngine; model: string; onChange: (engine: BotEngine, model: string) => void }) {
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
        <Field label="Model">
          <select className={inputCls} value={CLAUDE_MODELS.includes(model) ? model : "sonnet"} onChange={(e) => onChange(engine, e.target.value)}>
            {CLAUDE_MODELS.map((m) => <option key={m}>{m}</option>)}
          </select>
        </Field>
      ) : (
        <Field label="Model" hint="Any model your Codex CLI accepts.">
          <input className={inputCls + " mono"} list="codex-models" value={model} onChange={(e) => onChange(engine, e.target.value)} placeholder="gpt-5-codex" />
          <datalist id="codex-models">{CODEX_SUGGESTIONS.map((m) => <option key={m} value={m} />)}</datalist>
        </Field>
      )}
    </>
  );
}
