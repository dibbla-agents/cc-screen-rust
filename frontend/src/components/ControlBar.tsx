interface Props {
  onKey: (key: string) => void;
  disabled: boolean;
}

const KEY = "flex h-11 min-w-[2.75rem] items-center justify-center rounded-lg bg-panel px-3 text-sm font-medium text-slate-200 active:bg-edge disabled:opacity-40 select-none";

// On-screen keys for the things a soft keyboard does badly: arrow-menu
// navigation and one-shot control keys. These inject out-of-band via tmux
// send-keys (HTTP), so they work without the terminal having focus.
export default function ControlBar({ onKey, disabled }: Props) {
  const Btn = ({ k, label, cls = "" }: { k: string; label: string; cls?: string }) => (
    <button
      disabled={disabled}
      onMouseDown={(e) => e.preventDefault()} // keep focus on the terminal textarea; don't dismiss the soft keyboard
      onClick={() => onKey(k)}
      className={`${KEY} ${cls}`}
    >
      {label}
    </button>
  );

  // The trio you reach for most — Up, Down, Enter — is pinned on the right (thumb
  // side), ALWAYS visible in one tap. The secondary keys (⌃C, Esc, Tab, ⇧Tab,
  // ← →) scroll horizontally on the left, a swipe away when needed.
  return (
    <div className="flex items-center gap-1.5 border-t border-edge bg-bar px-2 py-2">
      <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-x-auto">
        <Btn k="c-c" label="⌃C" cls="text-amber" />
        <Btn k="escape" label="Esc" />
        <Btn k="tab" label="Tab" />
        <Btn k="btab" label="⇧Tab" />
        <span className="mx-0.5 h-7 w-px shrink-0 bg-edge" />
        <Btn k="left" label="←" />
        <Btn k="right" label="→" />
      </div>
      <span className="h-7 w-px shrink-0 bg-edge" />
      <Btn k="up" label="↑" cls="text-accent shrink-0" />
      <Btn k="down" label="↓" cls="text-accent shrink-0" />
      <Btn k="enter" label="⏎ Enter" cls="bg-accent text-bar font-semibold px-4 shrink-0" />
    </div>
  );
}
