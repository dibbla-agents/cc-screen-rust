import { useEffect, useState } from "react";
import { fetchDirs, type DirsResp } from "../api";
import { errMsg } from "./dirTree";

const dirnameOf = (p: string) => {
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
};
const basename = (p: string) => p.slice(p.lastIndexOf("/") + 1);

// MoveDialog — the touch-accessible "Move to…" folder picker (proposal 0012).
// HTML5 drag-and-drop doesn't fire on touch, so phones relocate a node by
// long-pressing it (context menu) → "Move to…", which opens this $HOME-scoped
// folder browser. "Move here" relocates the node into the folder currently
// shown. The destination is validated the same way as the drag guard + the
// backend: a no-op (already-parent) and illegal (into self / descendant) target
// disable the confirm.
export default function MoveDialog({
  src,
  startDir,
  machine,
  onConfirm,
  onClose,
}: {
  src: string; // the node being moved (a file or folder)
  startDir: string; // where the browser opens (the node's current parent)
  machine: string;
  onConfirm: (destDir: string) => Promise<void>; // throws → shown inline, dialog stays open
  onClose: () => void;
}) {
  const [resp, setResp] = useState<DirsResp | null>(null);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);

  const load = (path: string) => {
    setLoading(true);
    setErr("");
    fetchDirs(path, machine)
      .then(setResp)
      .catch((e) => setErr(errMsg(e)))
      .finally(() => setLoading(false));
  };
  useEffect(() => {
    load(startDir);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [startDir, machine]);

  const cur = resp?.path ?? startDir;
  const sameParent = dirnameOf(src) === cur;
  const intoSelf = cur === src || cur.startsWith(src + "/");
  const canMoveHere = !!resp && !sameParent && !intoSelf && !busy;

  const confirm = async () => {
    if (!canMoveHere) return;
    setBusy(true);
    setErr("");
    try {
      await onConfirm(cur);
      onClose();
    } catch (e) {
      setErr(errMsg(e));
      setBusy(false);
    }
  };

  const hint = sameParent
    ? "Already in this folder"
    : intoSelf
      ? "Can’t move a folder into itself"
      : `Move into ${basename(cur) || "/"}`;

  return (
    <>
      <div className="fixed inset-0 z-[85] bg-black/50" onClick={onClose} aria-hidden="true" />
      <div
        role="dialog"
        aria-label="Move to folder"
        className="fixed left-1/2 top-1/2 z-[86] flex max-h-[80vh] w-[min(92vw,28rem)] -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-2xl border border-edge bg-bar shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="shrink-0 border-b border-edge px-4 py-3">
          <div className="text-sm font-semibold text-slate-100">Move</div>
          <div className="truncate text-xs text-slate-400" title={src}>
            {basename(src)}
          </div>
        </div>

        {/* Current folder + up-one-level. */}
        <div className="flex shrink-0 items-center gap-2 border-b border-edge/60 px-3 py-2">
          <button
            onClick={() => resp && !resp.atHome && load(resp.parent)}
            disabled={!resp || resp.atHome || loading}
            className="shrink-0 rounded-md px-2 py-1 text-xs text-slate-300 ring-1 ring-inset ring-edge hover:bg-edge disabled:opacity-40"
            title="Up one folder"
          >
            ↑ Up
          </button>
          <code className="min-w-0 flex-1 truncate text-xs text-slate-400" title={cur}>
            {cur}
          </code>
        </div>

        {/* Subfolder list — tap to descend. */}
        <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1.5">
          {loading && <div className="px-2 py-6 text-center text-xs text-slate-500">Loading…</div>}
          {!loading && resp && resp.dirs.length === 0 && (
            <div className="px-2 py-6 text-center text-xs text-slate-600">No subfolders here.</div>
          )}
          {!loading &&
            resp?.dirs.map((d) => (
              <button
                key={d.path}
                onClick={() => load(d.path)}
                className="flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-left text-[13px] text-slate-200 hover:bg-edge"
              >
                <span className="shrink-0 text-slate-500">📁</span>
                <span className="min-w-0 flex-1 truncate">{d.name}</span>
                {d.path === src && <span className="shrink-0 text-[10px] text-slate-600">moving</span>}
                <span className="shrink-0 text-slate-600">›</span>
              </button>
            ))}
        </div>

        {err && <div className="shrink-0 px-4 py-2 text-xs text-red-400">{err}</div>}

        <div className="flex shrink-0 items-center gap-2 border-t border-edge px-3 py-2.5">
          <span className="min-w-0 flex-1 truncate text-[11px] text-slate-500">{hint}</span>
          <button
            onClick={onClose}
            disabled={busy}
            className="shrink-0 rounded-md px-3 py-1.5 text-xs text-slate-400 hover:text-slate-200"
          >
            Cancel
          </button>
          <button
            onClick={() => void confirm()}
            disabled={!canMoveHere}
            className="shrink-0 rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar disabled:bg-panel disabled:text-slate-500"
          >
            {busy ? "Moving…" : "Move here"}
          </button>
        </div>
      </div>
    </>
  );
}
