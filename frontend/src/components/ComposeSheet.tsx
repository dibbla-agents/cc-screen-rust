import { forwardRef, useImperativeHandle, useMemo, useRef, useState } from "react";
import type { Favorite } from "../api";
import { loadRecents, rememberRecent } from "../recents";
import { StarIcon } from "../icons";

export interface ComposeHandle {
  focus: () => void;
}

interface Props {
  open: boolean;
  onClose: () => void;
  onSend: (text: string, enter: boolean) => void;
  favorites: Favorite[];
}

// The out-of-band prompt composer: write comfortably here (native phone editing,
// autocorrect, multi-line) and inject the whole block via bracketed paste —
// never fight the cursor inside the live TUI. "Send" leaves the text for you to
// eyeball; "Send ⏎" submits it.
//
// It stays mounted (just hidden when closed) so the opener can call focus()
// synchronously inside the tap — the only way iOS Safari will raise the soft
// keyboard. A setTimeout/effect focus fires outside the gesture and is ignored.
const ComposeSheet = forwardRef<ComposeHandle, Props>(function ComposeSheet(
  { open, onClose, onSend, favorites },
  ref
) {
  const [text, setText] = useState("");
  const [recents, setRecents] = useState<string[]>(loadRecents);
  const taRef = useRef<HTMLTextAreaElement>(null);

  useImperativeHandle(ref, () => ({ focus: () => taRef.current?.focus() }), []);

  // Autocomplete: as you type the prompt, surface matching favourites + recents
  // (favourites first), the 6 most relevant, updated on every keystroke. Tap one
  // to drop the full text into the box. Empty query => freshest suggestions.
  const suggestions = useMemo(() => {
    const q = text.trim().toLowerCase();
    const hit = (s: string) => !q || s.toLowerCase().includes(q);
    const favTexts = new Set(favorites.map((f) => f.text));
    const fav = favorites.filter((f) => hit(f.text)).map((f) => ({ text: f.text, fav: true }));
    const rec = recents
      .filter((r) => hit(r) && !favTexts.has(r))
      .map((r) => ({ text: r, fav: false }));
    return [...fav, ...rec].filter((s) => s.text !== text).slice(0, 6);
  }, [text, favorites, recents]);

  const submit = (enter: boolean) => {
    const t = text;
    if (!t.trim()) return;
    onSend(t, enter);
    setRecents(rememberRecent(t));
    setText("");
    onClose();
  };

  return (
    <div
      className={`absolute inset-0 z-40 flex flex-col justify-end transition-opacity duration-150 ${
        open ? "opacity-100" : "pointer-events-none opacity-0"
      }`}
    >
      <div className="flex-1 bg-black/50" onClick={onClose} />
      <div className="rounded-t-2xl border-t border-edge bg-panel p-3 pb-safe">
        <div className="mx-auto mb-2 h-1 w-10 rounded-full bg-edge" />

        {suggestions.length > 0 && (
          // Live search results over favourites + recents — scroll sideways.
          <div className="mb-2 flex flex-nowrap gap-1.5 overflow-x-auto overscroll-x-contain pb-1">
            {suggestions.map((s, i) => (
              <button
                key={i}
                onClick={() => setText(s.text)}
                className="flex shrink-0 items-center gap-1 rounded-full bg-bar px-3 py-1 text-xs text-slate-400 active:bg-edge"
                style={{ maxWidth: "12rem" }}
              >
                {s.fav && <StarIcon filled className="h-3 w-3 shrink-0 text-amber" />}
                <span className="truncate">{s.text}</span>
              </button>
            ))}
          </div>
        )}

        <textarea
          ref={taRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={4}
          placeholder="Write a prompt — sent as one block, no fighting the cursor…"
          className="w-full resize-none rounded-lg border border-edge bg-bar p-3 font-mono text-[15px] text-slate-100 outline-none focus:border-accent"
        />

        <div className="mt-2 flex gap-2">
          <button
            onClick={onClose}
            className="rounded-lg bg-bar px-4 py-3 text-sm text-slate-400 active:bg-edge"
          >
            Cancel
          </button>
          <button
            onClick={() => submit(false)}
            className="flex-1 rounded-lg bg-edge px-4 py-3 text-sm font-medium text-slate-100 active:opacity-80"
          >
            Send
          </button>
          <button
            onClick={() => submit(true)}
            className="flex-1 rounded-lg bg-accent px-4 py-3 text-sm font-semibold text-bar active:opacity-80"
          >
            Send ⏎
          </button>
        </div>
      </div>
    </div>
  );
});

export default ComposeSheet;
