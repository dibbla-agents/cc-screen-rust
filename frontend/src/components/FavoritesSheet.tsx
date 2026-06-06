import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import type { Favorite } from "../api";
import { loadRecents } from "../recents";
import { PencilIcon, StarIcon } from "../icons";

export interface FavoritesHandle {
  focus: () => void;
}

interface Props {
  open: boolean;
  onClose: () => void;
  favorites: Favorite[];
  // Inject a prompt: parent pastes it into the agent AND submits it (Enter), then
  // closes the sheet. One tap fires it straight at the agent.
  onInject: (text: string) => void;
  onAdd: (text: string) => void;
  onUpdate: (id: string, text: string) => void;
  onDelete: (id: string) => void;
}

// editing === null         -> no editor open
// editing.id === null      -> composing a NEW favourite
// editing.id === "<id>"    -> editing that favourite
type Editing = { id: string | null } | null;

// The favourites hub: a searchable list of saved prompts, plus your send-history
// to promote from. Optimised for the two hot paths:
//   • fire    — tap a row's text -> it's sent straight at the agent (Enter). 1 tap.
//   • curate  — ☆ on a history row saves it; the search box doubles as "add new".
const FavoritesSheet = forwardRef<FavoritesHandle, Props>(function FavoritesSheet(
  { open, onClose, favorites, onInject, onAdd, onUpdate, onDelete },
  ref
) {
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<Editing>(null);
  const [draft, setDraft] = useState("");
  const draftRef = useRef<HTMLTextAreaElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  // preventScroll on every focus so raising the keyboard / opening the editor
  // never scrolls the overflow:hidden app shell out of view. See proposals/P0002.
  useImperativeHandle(
    ref,
    () => ({ focus: () => searchRef.current?.focus({ preventScroll: true }) }),
    []
  );

  // The sheet stays mounted (just hidden when closed) so the opener can focus the
  // search box synchronously inside the tap — the only way iOS Safari raises the
  // soft keyboard. Reset transient state whenever it closes.
  useEffect(() => {
    if (!open) {
      setQuery("");
      setEditing(null);
      setDraft("");
    }
  }, [open]);

  const q = query.trim().toLowerCase();
  const match = (t: string) => !q || t.toLowerCase().includes(q);

  const favs = favorites.filter((f) => match(f.text));
  const exact = favorites.some((f) => f.text.trim() === query.trim());
  // History = recents you haven't favourited yet (and that match the search).
  const favTexts = new Set(favorites.map((f) => f.text));
  const history = loadRecents().filter((t) => !favTexts.has(t) && match(t));

  const startAdd = () => {
    setEditing({ id: null });
    setDraft(query.trim()); // seed from the search box if you typed something
    setTimeout(() => draftRef.current?.focus({ preventScroll: true }), 0);
  };
  const startEdit = (f: Favorite) => {
    setEditing({ id: f.id });
    setDraft(f.text);
    setTimeout(() => draftRef.current?.focus({ preventScroll: true }), 0);
  };
  const saveEditor = () => {
    const t = draft.trim();
    if (!t) {
      setEditing(null);
      return;
    }
    if (editing?.id) onUpdate(editing.id, t);
    else onAdd(t);
    setEditing(null);
    setDraft("");
    setQuery("");
  };

  return (
    <div
      className={`absolute inset-0 z-40 flex flex-col justify-end transition-opacity duration-150 ${
        open ? "opacity-100" : "pointer-events-none opacity-0"
      }`}
    >
      <div className="flex-1 bg-black/50" onClick={onClose} />
      {/* min-h-0 lets the max-h cap win over a flex item's default min-height:auto
          (otherwise a tall list grows the panel past the screen and never scrolls). */}
      <div className="flex max-h-[88%] min-h-0 flex-col rounded-t-2xl border-t border-edge bg-panel pb-safe">
        <div className="shrink-0 px-3 pt-3">
          <div className="mx-auto mb-2 h-1 w-10 rounded-full bg-edge" />
          <div className="flex items-center gap-2">
            <input
              ref={searchRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search favourites…"
              className="min-w-0 flex-1 rounded-lg border border-edge bg-bar px-3 py-2.5 text-sm text-slate-100 outline-none focus:border-accent"
            />
            <button
              onClick={startAdd}
              className="shrink-0 rounded-lg bg-edge px-3 py-2.5 text-sm font-medium text-slate-100 active:opacity-80"
            >
              ＋ New
            </button>
          </div>

          {editing && (
            <div className="mt-2 rounded-lg border border-accent/60 bg-bar p-2">
              <textarea
                ref={draftRef}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                rows={3}
                placeholder="Favourite prompt text…"
                className="w-full resize-none rounded-md bg-panel p-2 font-mono text-[14px] text-slate-100 outline-none"
              />
              <div className="mt-2 flex gap-2">
                <button
                  onClick={() => {
                    setEditing(null);
                    setDraft("");
                  }}
                  className="rounded-md bg-panel px-3 py-2 text-xs text-slate-400 active:bg-edge"
                >
                  Cancel
                </button>
                {editing.id && (
                  <button
                    onClick={() => {
                      onDelete(editing.id!);
                      setEditing(null);
                    }}
                    className="rounded-md bg-red-500/80 px-3 py-2 text-xs font-semibold text-bar active:opacity-80"
                  >
                    Delete
                  </button>
                )}
                <button
                  onClick={saveEditor}
                  className="flex-1 rounded-md bg-accent px-3 py-2 text-xs font-semibold text-bar active:opacity-80"
                >
                  Save
                </button>
              </div>
            </div>
          )}
        </div>

        <div className="mt-2 min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 pb-1">
          {/* Quick-add the typed query when it isn't already a favourite. */}
          {query.trim() && !exact && !editing && (
            <button
              onClick={() => {
                onAdd(query.trim());
                setQuery("");
              }}
              className="mb-1 flex w-full items-center gap-2 rounded-lg px-2 py-2.5 text-left text-sm text-accent active:bg-bar"
            >
              <span className="text-base leading-none">＋</span>
              <span className="truncate">
                Add “{query.trim()}” to favourites
              </span>
            </button>
          )}

          {favs.map((f) => (
            <div key={f.id} className="flex items-stretch gap-1 border-b border-edge/40">
              <button
                onClick={() => onInject(f.text)}
                className="flex min-w-0 flex-1 items-start gap-2 px-2 py-2.5 text-left active:bg-bar"
                title="Send this prompt to the agent"
              >
                <StarIcon filled className="mt-0.5 h-4 w-4 shrink-0 text-amber" />
                <span className="min-w-0 whitespace-pre-wrap break-words text-sm text-slate-100 line-clamp-3">
                  {f.text}
                </span>
              </button>
              <button
                onClick={() => startEdit(f)}
                aria-label="Edit favourite"
                className="flex shrink-0 items-center px-3 text-slate-500 active:text-slate-200"
              >
                <PencilIcon className="h-4 w-4" />
              </button>
            </div>
          ))}

          {favs.length === 0 && !query.trim() && (
            <div className="px-2 py-6 text-center text-sm text-slate-500">
              No favourites yet. Tap ＋ New, or ☆ a prompt from history below.
            </div>
          )}

          {history.length > 0 && (
            <>
              <div className="px-2 pb-1 pt-3 text-[11px] uppercase tracking-wide text-slate-600">
                From history — tap ☆ to save
              </div>
              {history.map((t, i) => (
                <div key={i} className="flex items-stretch gap-1 border-b border-edge/30">
                  <button
                    onClick={() => onInject(t)}
                    className="flex min-w-0 flex-1 items-start gap-2 px-2 py-2.5 text-left active:bg-bar"
                    title="Send this prompt to the agent"
                  >
                    <span className="min-w-0 whitespace-pre-wrap break-words text-sm text-slate-400 line-clamp-2">
                      {t}
                    </span>
                  </button>
                  <button
                    onClick={() => onAdd(t)}
                    aria-label="Save to favourites"
                    className="flex shrink-0 items-center px-3 text-slate-500 active:text-amber"
                  >
                    <StarIcon className="h-4 w-4" />
                  </button>
                </div>
              ))}
            </>
          )}
        </div>

        <div className="shrink-0 px-3 pb-2 pt-2">
          <button
            onClick={onClose}
            className="w-full rounded-lg bg-bar px-4 py-3 text-sm text-slate-400 active:bg-edge"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
});

export default FavoritesSheet;
