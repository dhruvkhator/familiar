import { ACCESSORIES, EYE_COUNT, MOUTH_COUNT, Mascot, PALETTE, SHAPE_COUNT, hueToHex, randomAvatar, type Accessory, type Avatar } from "./Mascot";
import { Button, cx } from "./ui";

function Opt({ on, label, onClick, children }: { on: boolean; label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <button type="button" onClick={onClick} aria-label={label} aria-pressed={on}
      className={cx("rounded-[12px] border p-1 cursor-pointer min-w-11 min-h-11 flex items-center justify-center", on ? "border-accent bg-accent-soft" : "border-line hover:bg-sunken")}>
      {children}
    </button>
  );
}

const range = (n: number) => Array.from({ length: n }, (_, i) => i);
const label = (a: Accessory) => (a === "none" ? "None" : a[0].toUpperCase() + a.slice(1));

/** Compose an avatar from parts. `id` seeds nothing visual here: the value is fully explicit. */
export function AvatarBuilder({ value, onChange, name }: { value: Avatar; onChange: (a: Avatar) => void; name: string }) {
  const set = (p: Partial<Avatar>) => onChange({ ...value, ...p });
  const mini = (p: Partial<Avatar>) => <Mascot id="builder" name="option" avatar={{ ...value, ...p }} size={34} />;
  const isCustom = !PALETTE.includes(value.color as (typeof PALETTE)[number]);
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <Mascot id="builder-preview" name={name || "Your teammate"} avatar={value} size={110} state="idle" />
        <Button type="button" onClick={() => onChange(randomAvatar())}>Randomize</Button>
      </div>
      <fieldset>
        <legend className="text-sm font-medium mb-1">Shape</legend>
        <div className="flex flex-wrap gap-1.5">{range(SHAPE_COUNT).map((i) => <Opt key={i} on={value.shape === i} label={`Shape ${i + 1}`} onClick={() => set({ shape: i })}>{mini({ shape: i })}</Opt>)}</div>
      </fieldset>
      <fieldset>
        <legend className="text-sm font-medium mb-1">Color</legend>
        <div className="flex flex-wrap items-center gap-2">
          {PALETTE.map((c, i) => (
            <button type="button" key={c} onClick={() => set({ color: c })} aria-label={`Color ${i + 1}`} aria-pressed={value.color === c}
              className={cx("size-9 rounded-full cursor-pointer border-2", value.color === c ? "border-ink" : "border-transparent")} style={{ background: c }} />
          ))}
          <label className="flex items-center gap-2 text-xs text-muted ml-2">
            Custom
            <input type="range" min={0} max={359} aria-label="Custom hue" defaultValue={200} onChange={(e) => set({ color: hueToHex(+e.target.value) })}
              className={cx("w-28 accent-accent", isCustom && "opacity-100")} />
          </label>
        </div>
      </fieldset>
      <fieldset>
        <legend className="text-sm font-medium mb-1">Eyes</legend>
        <div className="flex flex-wrap gap-1.5">{range(EYE_COUNT).map((i) => <Opt key={i} on={value.eyes === i} label={`Eyes ${i + 1}`} onClick={() => set({ eyes: i })}>{mini({ eyes: i })}</Opt>)}</div>
      </fieldset>
      <fieldset>
        <legend className="text-sm font-medium mb-1">Expression</legend>
        <div className="flex flex-wrap gap-1.5">{range(MOUTH_COUNT).map((i) => <Opt key={i} on={value.mouth === i} label={`Expression ${i + 1}`} onClick={() => set({ mouth: i })}>{mini({ mouth: i })}</Opt>)}</div>
      </fieldset>
      <fieldset>
        <legend className="text-sm font-medium mb-1">Accessory</legend>
        <div className="flex flex-wrap gap-1.5">{ACCESSORIES.map((k) => <Opt key={k} on={value.accessory === k} label={label(k)} onClick={() => set({ accessory: k })}>{mini({ accessory: k })}</Opt>)}</div>
      </fieldset>
    </div>
  );
}
