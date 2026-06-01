import { useLayoutEffect, useRef, useState } from "react";

// What the menu acts on. A "dir" with root:true is a section root (Share /
// Project / Home) — those can't be renamed or deleted, only added into.
export type CtxTarget =
  | { kind: "file"; path: string; name: string }
  | { kind: "dir"; path: string; name: string; root?: boolean };

export interface CtxHandlers {
  onOpenFile: (path: string) => void;
  onDownload: (path: string, name: string) => void;
  onNewFile: (dir: string, name: string) => Promise<void>;
  onNewFolder: (dir: string, name: string) => Promise<void>;
  onRename: (path: string, name: string) => Promise<void>;
  onDeleteFile: (path: string) => Promise<void>;
  onDeleteFolder: (path: string) => Promise<void>; // recursive
}

interface Props extends CtxHandlers {
  x: number;
  y: number;
  target: CtxTarget;
  onClose: () => void;
}

type Mode =
  | { t: "menu" }
  | { t: "input"; action: "newfile" | "newfolder" | "rename" }
  | { t: "confirm" };

// ContextMenu — a positioned popup for file-tree CRUD. Opened by right-click
// (desktop) or long-press (touch). "New file / New folder / Rename" switch the
// popup into an inline text field; "Delete" switches it into a confirm. The
// async work itself is the editor's (it owns the API calls + tree refresh); this
// component only drives the little flow and reports errors inline. Esc and
// outside-click dismissal are handled by the parent (EditorOverlay peels the
// menu off first in its capture-phase Esc handler) plus the catcher below.
export default function ContextMenu({
  x,
  y,
  target,
  onClose,
  onOpenFile,
  onDownload,
  onNewFile,
  onNewFolder,
  onRename,
  onDeleteFile,
  onDeleteFolder,
}: Props) {
  const [mode, setMode] = useState<Mode>({ t: "menu" });
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  // Keep the popup on-screen: measure after each mode change (the input/confirm
  // views are a different size) and nudge it inside the viewport.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const pad = 8;
    let left = x;
    let top = y;
    if (left + r.width > window.innerWidth - pad) left = Math.max(pad, window.innerWidth - r.width - pad);
    if (top + r.height > window.innerHeight - pad) top = Math.max(pad, window.innerHeight - r.height - pad);
    setPos({ left, top });
  }, [x, y, mode.t]);

  // Focus + select the field whenever we enter input mode.
  useLayoutEffect(() => {
    if (mode.t === "input") inputRef.current?.select();
  }, [mode.t]);

  const startInput = (action: "newfile" | "newfolder" | "rename") => {
    setErr("");
    setValue(action === "rename" ? target.name : "");
    setMode({ t: "input", action });
  };

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setErr("");
    try {
      await fn();
      onClose();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const submitInput = () => {
    if (mode.t !== "input") return;
    const name = value.trim();
    if (!name) return;
    if (mode.action === "newfile") void run(() => onNewFile(target.path, name));
    else if (mode.action === "newfolder") void run(() => onNewFolder(target.path, name));
    else void run(() => onRename(target.path, name));
  };

  const confirmDelete = () =>
    void run(() =>
      target.kind === "file" ? onDeleteFile(target.path) : onDeleteFolder(target.path)
    );

  const itemCls =
    "flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[13px] text-slate-200 hover:bg-edge disabled:opacity-50";

  return (
    <>
      {/* Outside-tap / right-click catcher. */}
      <div
        className="fixed inset-0 z-[79]"
        onClick={onClose}
        onContextMenu={(e) => {
          e.preventDefault();
          onClose();
        }}
        aria-hidden="true"
      />
      <div
        ref={ref}
        role="menu"
        className="fixed z-[80] min-w-[12rem] max-w-[16rem] rounded-lg border border-edge bg-bar/98 p-1 shadow-2xl backdrop-blur"
        style={{ left: pos.left, top: pos.top }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="truncate px-2.5 py-1 text-[11px] font-medium text-slate-500" title={target.name}>
          {target.kind === "dir" ? "📁 " : ""}
          {target.name}
        </div>

        {mode.t === "menu" && (
          <div className="flex flex-col">
            {target.kind === "file" && (
              <>
                <button className={itemCls} onClick={() => { onOpenFile(target.path); onClose(); }}>
                  Open
                </button>
                <button className={itemCls} onClick={() => { onDownload(target.path, target.name); onClose(); }}>
                  Download
                </button>
                <button className={itemCls} onClick={() => startInput("rename")}>
                  Rename…
                </button>
                <button className={`${itemCls} text-red-400 hover:bg-red-500/10`} onClick={() => { setErr(""); setMode({ t: "confirm" }); }}>
                  Delete
                </button>
              </>
            )}
            {target.kind === "dir" && (
              <>
                <button className={itemCls} onClick={() => startInput("newfile")}>
                  New file…
                </button>
                <button className={itemCls} onClick={() => startInput("newfolder")}>
                  New folder…
                </button>
                {!target.root && (
                  <>
                    <button className={itemCls} onClick={() => startInput("rename")}>
                      Rename…
                    </button>
                    <button className={`${itemCls} text-red-400 hover:bg-red-500/10`} onClick={() => { setErr(""); setMode({ t: "confirm" }); }}>
                      Delete folder
                    </button>
                  </>
                )}
              </>
            )}
          </div>
        )}

        {mode.t === "input" && (
          <div className="p-1.5">
            <input
              ref={inputRef}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") { e.preventDefault(); submitInput(); }
                else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); }
              }}
              placeholder={mode.action === "newfolder" ? "folder name" : "file name"}
              disabled={busy}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              className="w-full rounded-md border border-edge bg-panel px-2 py-1.5 text-[13px] text-slate-100 outline-none focus:border-accent"
            />
            <div className="mt-1.5 flex justify-end gap-1.5">
              <button className="rounded-md px-2 py-1 text-[12px] text-slate-400 hover:text-slate-200" onClick={onClose} disabled={busy}>
                Cancel
              </button>
              <button className="rounded-md bg-accent px-2.5 py-1 text-[12px] font-semibold text-bar disabled:opacity-50" onClick={submitInput} disabled={busy || !value.trim()}>
                {busy ? "…" : mode.action === "rename" ? "Rename" : "Create"}
              </button>
            </div>
          </div>
        )}

        {mode.t === "confirm" && (
          <div className="p-1.5">
            <div className="px-1 py-1 text-[12px] text-slate-300">
              {target.kind === "file" ? (
                <>Delete <span className="font-medium text-slate-100">{target.name}</span>?</>
              ) : (
                <>Delete <span className="font-medium text-slate-100">{target.name}</span> and everything inside it?</>
              )}
            </div>
            <div className="mt-1.5 flex justify-end gap-1.5">
              <button className="rounded-md px-2 py-1 text-[12px] text-slate-400 hover:text-slate-200" onClick={onClose} disabled={busy}>
                Cancel
              </button>
              <button className="rounded-md bg-red-500/90 px-2.5 py-1 text-[12px] font-semibold text-bar disabled:opacity-50" onClick={confirmDelete} disabled={busy}>
                {busy ? "…" : "Delete"}
              </button>
            </div>
          </div>
        )}

        {err && <div className="px-2.5 py-1 text-[11px] text-red-400">{err}</div>}
      </div>
    </>
  );
}
