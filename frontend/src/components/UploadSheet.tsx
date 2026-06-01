import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  checkUpload,
  fetchDirs,
  makeDir,
  sessionRoot,
  uploadFiles,
  type DirEntry,
  type UploadFile,
  type UploadMode,
  type UploadResult,
} from "../api";

interface Props {
  open: boolean;
  session: string | null;
  files: UploadFile[];
  onClose: () => void;
  onResult: (r: UploadResult) => void;
}

function basename(p: string): string {
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] || "/";
}

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Group dropped files by top-level folder for the summary view: a flat
// "src/icons/x.svg" + "src/icons/y.svg" + "README.md" drop collapses to
// `src/ (2 files, 1.2 KB)` + `README.md (348 B)`, which is far less wall of
// text in the sheet header. Single files keep their own row.
interface UploadGroup {
  label: string;     // "src/" for folders, "x.svg" for top-level files
  isFolder: boolean;
  fileCount: number;
  totalSize: number;
  paths: string[];
}

function groupFiles(files: UploadFile[]): UploadGroup[] {
  const byTop = new Map<string, UploadGroup>();
  for (const f of files) {
    const i = f.relPath.indexOf("/");
    const top = i < 0 ? f.relPath : f.relPath.slice(0, i);
    const isFolder = i >= 0;
    let g = byTop.get(top);
    if (!g) {
      g = {
        label: isFolder ? top + "/" : top,
        isFolder,
        fileCount: 0,
        totalSize: 0,
        paths: [],
      };
      byTop.set(top, g);
    }
    g.fileCount++;
    g.totalSize += f.file.size;
    g.paths.push(f.relPath);
  }
  return Array.from(byTop.values());
}

// UploadSheet — drop landing modal. Three phases woven into one component
// because they share state (the file list, the destination dir, the
// conflict map):
//   1. Pick destination  — expandable tree rooted at the session's project
//                          root. Selection (the "Upload here") and expansion
//                          are independent: a row's chevron toggles its
//                          children, the row body picks it as the
//                          destination — so you can peek inside a folder
//                          without committing to it. Lazy-loads each
//                          folder's children on first expand, cached for
//                          the lifetime of the sheet.
//   2. Resolve conflicts — if the pre-flight check turns up colliding
//                          names, show them with per-file Rename /
//                          Overwrite / Skip chips and an apply-to-all row.
//   3. Upload            — one streamed multipart POST with the manifest
//                          and every non-skipped file. Total-bytes
//                          progress bar feeds from xhr.upload.onprogress.
export default function UploadSheet({
  open,
  session,
  files,
  onClose,
  onResult,
}: Props) {
  // Tree anchor + tree state. Cache is keyed by absolute path; expanded
  // and loading are sets keyed the same way. Mirrors the dir-tree idiom in
  // dirTree.tsx so someone reading both files doesn't have to context-switch.
  const [root, setRoot] = useState<string | null>(null);
  const [home, setHome] = useState<string>("");
  const [cache, setCache] = useState<Map<string, DirEntry[]>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState<Set<string>>(new Set());
  // Destination is the folder the upload will land in. Defaults to root
  // when the sheet opens; persists across child-folder expansions so the
  // user can browse without losing their pick.
  const [selectedDir, setSelectedDir] = useState<string | null>(null);

  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [opErr, setOpErr] = useState<string | null>(null);

  // Pre-flight conflict state.
  const [conflicts, setConflicts] = useState<string[]>([]);
  const [resolution, setResolution] = useState<Record<string, UploadMode>>({});

  // Upload phase.
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState(0);
  const [err, setErr] = useState<string | null>(null);
  const cancelledRef = useRef(false);

  const groups = useMemo(() => groupFiles(files), [files]);
  const totalSize = useMemo(
    () => files.reduce((s, f) => s + f.file.size, 0),
    [files]
  );

  // loadChildren fetches a folder's subdirs (lazy on first expand) and
  // puts them in the cache. Idempotent — repeated calls short-circuit if
  // already cached. The `loading` set drives the per-row spinner so the
  // tree never has a "frozen chevron" moment.
  const loadChildren = useCallback(
    async (path: string) => {
      if (cache.has(path) || loading.has(path)) return;
      setLoading((s) => new Set(s).add(path));
      try {
        const d = await fetchDirs(path);
        setCache((c) => {
          const next = new Map(c);
          next.set(path, d.dirs);
          return next;
        });
      } catch (e) {
        setOpErr(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading((s) => {
          const n = new Set(s);
          n.delete(path);
          return n;
        });
      }
    },
    [cache, loading]
  );

  // toggle flips a node's expansion. Lazy-loads if first-time. Used by the
  // chevron click — see selectNode for the row-body click that also
  // expands as a side-effect.
  const toggle = useCallback(
    (path: string) => {
      setExpanded((prev) => {
        const next = new Set(prev);
        if (next.has(path)) {
          next.delete(path);
        } else {
          next.add(path);
          if (!cache.has(path)) loadChildren(path);
        }
        return next;
      });
    },
    [cache, loadChildren]
  );

  // selectNode picks a folder as the upload destination. Also expands it
  // (and lazy-loads if needed), because "show me what's inside the folder
  // I just picked" is almost always the next thing the user wants — and
  // collapsing is still available via the chevron.
  const selectNode = useCallback(
    (path: string) => {
      setSelectedDir(path);
      setExpanded((prev) => {
        if (prev.has(path)) return prev;
        const next = new Set(prev).add(path);
        if (!cache.has(path)) loadChildren(path);
        return next;
      });
    },
    [cache, loadChildren]
  );

  // Phase reset on open.
  useEffect(() => {
    if (!open) return;
    cancelledRef.current = false;
    setErr(null);
    setOpErr(null);
    setUploading(false);
    setProgress(0);
    setCreating(false);
    setNewName("");
    setConflicts([]);
    setResolution({});
    setCache(new Map());
    setExpanded(new Set());
    setLoading(new Set());
    setSelectedDir(null);
    setRoot(null);
  }, [open]);

  // Anchor the tree at the session's project root, default destination to
  // it, and eagerly load its children so the tree isn't empty on first
  // render. Single effect for both reads — they're a unit.
  useEffect(() => {
    if (!open || !session) return;
    let cancelled = false;
    (async () => {
      try {
        const { root: r, home: h } = await sessionRoot(session);
        if (cancelled) return;
        setRoot(r);
        setHome(h);
        setSelectedDir(r);
        setExpanded(new Set([r]));
        // Load the root's children eagerly so the tree isn't a single line
        // on open. Subsequent levels load lazily on expand.
        const d = await fetchDirs(r);
        if (cancelled) return;
        setCache((c) => new Map(c).set(r, d.dirs));
      } catch (e) {
        if (!cancelled) setErr(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, session]);

  // Pre-flight collision check whenever the selected destination changes.
  // Server returns the subset of names already present; we default the
  // resolution to "rename" (safer — never destroys).
  useEffect(() => {
    if (!open || !session || !selectedDir) return;
    let cancelled = false;
    const names = files.map((f) => f.relPath);
    checkUpload(session, selectedDir, names)
      .then(({ exists }) => {
        if (cancelled) return;
        setConflicts(exists);
        setResolution((prev) => {
          // Preserve user choices for names that are still in conflict;
          // default brand-new conflicts to "rename".
          const next: Record<string, UploadMode> = {};
          for (const n of exists) next[n] = prev[n] ?? "rename";
          return next;
        });
      })
      .catch(() => {
        if (cancelled) return;
        // Pre-flight failure isn't fatal — the server will still apply
        // the default mode at upload time. Clear local state so the UI
        // doesn't show stale conflicts.
        setConflicts([]);
        setResolution({});
      });
    return () => {
      cancelled = true;
    };
  }, [open, session, selectedDir, files]);

  const createFolder = async () => {
    if (!selectedDir || !newName.trim()) return;
    try {
      await makeDir(selectedDir, newName.trim());
      setCreating(false);
      setNewName("");
      // Invalidate this folder's children cache and re-fetch so the new
      // folder appears. Keep the selection on the parent so the user can
      // tap into the new folder if they want it as the destination.
      setCache((c) => {
        const next = new Map(c);
        next.delete(selectedDir);
        return next;
      });
      await loadChildren(selectedDir);
    } catch (e) {
      setOpErr(e instanceof Error ? e.message : String(e));
    }
  };

  const setAllConflicts = (mode: UploadMode) => {
    const next: Record<string, UploadMode> = {};
    for (const n of conflicts) next[n] = mode;
    setResolution(next);
  };

  const upload = async () => {
    if (!session || !selectedDir) return;
    const toSend = files.filter((f) => resolution[f.relPath] !== "skip");
    if (toSend.length === 0) {
      onClose();
      return;
    }
    setUploading(true);
    setErr(null);
    setProgress(0);
    try {
      const result = await uploadFiles(
        session,
        selectedDir,
        toSend,
        resolution,
        (frac) => {
          if (!cancelledRef.current) setProgress(frac);
        }
      );
      if (cancelledRef.current) return;
      onResult(result);
    } catch (e) {
      if (!cancelledRef.current) {
        setErr(e instanceof Error ? e.message : String(e));
        setUploading(false);
      }
    }
  };

  const close = () => {
    cancelledRef.current = true;
    onClose();
  };

  if (!open) return null;

  // Display helpers — turn an absolute path into "~/.../short/form".
  const rel = (p: string) => {
    if (!p) return "";
    if (root && p === root) return basename(root);
    if (root && p.startsWith(root + "/")) return p.slice(root.length + 1);
    if (home && p === home) return "~";
    if (home && p.startsWith(home + "/")) return "~" + p.slice(home.length);
    return p;
  };
  const sendCount = files.filter((f) => resolution[f.relPath] !== "skip").length;
  const skipCount = files.length - sendCount;
  // The "apply to all" chip is highlighted only when every conflict already
  // matches that mode — we infer it from the resolution map, so the chip
  // also un-highlights the moment the user tweaks a single row.
  const uniqueModes = new Set(conflicts.map((n) => resolution[n] ?? "rename"));
  const allSame = uniqueModes.size === 1 ? [...uniqueModes][0] : null;

  return (
    <div className="absolute inset-0 z-50 flex flex-col bg-bar pt-safe">
      <div className="flex items-center gap-3 border-b border-edge px-4 py-3">
        <span className="flex-1 truncate text-lg font-semibold text-slate-100">
          Upload {files.length} {files.length === 1 ? "file" : "files"}
          <span className="ml-2 text-sm font-normal text-slate-500">
            ({fmtSize(totalSize)})
          </span>
        </span>
        <button
          onClick={close}
          className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge"
        >
          ✕
        </button>
      </div>

      {err && (
        <div className="border-b border-red-500/20 bg-red-500/10 px-4 py-2 text-sm text-red-400">
          {err}
        </div>
      )}

      {/* What's being uploaded — grouped by top-level folder. */}
      <div className="border-b border-edge/40 bg-panel/40 px-4 py-2 text-sm">
        <div className="mb-1 text-xs uppercase tracking-wider text-slate-500">
          What
        </div>
        <ul className="flex flex-wrap gap-x-4 gap-y-1 text-slate-200">
          {groups.map((g) => (
            <li key={g.label} className="flex items-center gap-1.5">
              <span className="text-slate-500">{g.isFolder ? "📁" : "📄"}</span>
              <span className="font-mono text-[13px]">{g.label}</span>
              {g.fileCount > 1 && (
                <span className="text-xs text-slate-500">
                  ({g.fileCount} files, {fmtSize(g.totalSize)})
                </span>
              )}
              {g.fileCount === 1 && !g.isFolder && (
                <span className="text-xs text-slate-500">
                  ({fmtSize(g.totalSize)})
                </span>
              )}
            </li>
          ))}
        </ul>
      </div>

      {/* Destination header. Shows the live selection (so the tree below
          can scroll without losing it from view) and the new-folder action.
          The path is rendered relative to the project root, so it stays
          terse: "" (= root) / "src" / "src/icons" / etc. */}
      <div className="flex items-center gap-2 border-b border-edge/60 px-3 py-2">
        <span className="shrink-0 text-xs uppercase tracking-wider text-slate-500">
          Into
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-[13px] text-slate-200">
          {selectedDir ? rel(selectedDir) : "…"}
        </span>
        <button
          onClick={() => {
            setOpErr(null);
            setNewName("");
            setCreating(true);
          }}
          disabled={!selectedDir}
          title={selectedDir ? `New folder in ${rel(selectedDir)}` : ""}
          className="shrink-0 rounded-md bg-panel px-3 py-1.5 text-sm text-slate-300 active:bg-edge disabled:opacity-30"
        >
          ＋📁
        </button>
      </div>

      {opErr && (
        <div className="border-b border-edge/40 px-4 py-2 text-sm text-red-400">
          {opErr}
        </div>
      )}

      {creating && (
        <div className="flex items-center gap-2 border-b border-edge/40 bg-panel/40 px-3 py-2">
          <span className="text-xs text-slate-500">in</span>
          <span className="truncate font-mono text-[12px] text-slate-400">
            {selectedDir ? rel(selectedDir) || "(root)" : "…"}
          </span>
          <span className="text-slate-500">/</span>
          <input
            autoFocus
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") createFolder();
              if (e.key === "Escape") {
                setCreating(false);
                setNewName("");
              }
            }}
            placeholder="new folder name"
            className="min-w-0 flex-1 rounded-md border border-edge bg-bar px-2 py-1.5 text-sm text-slate-100 outline-none focus:border-accent"
          />
          <button
            onClick={() => {
              setCreating(false);
              setNewName("");
            }}
            className="px-2 py-1.5 text-slate-400"
          >
            ✕
          </button>
          <button
            onClick={createFolder}
            disabled={!newName.trim()}
            className="rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-bar disabled:opacity-40"
          >
            Add
          </button>
        </div>
      )}

      {/* Tree + conflicts share one scroll region so a long tree doesn't
          push the conflicts panel off-screen at small heights. */}
      <div className="flex-1 overflow-y-auto">
        {root && (
          <ul className="py-1">
            <FolderRow
              path={root}
              name={basename(root)}
              depth={0}
              cache={cache}
              expanded={expanded}
              loading={loading}
              selectedDir={selectedDir}
              onToggle={toggle}
              onSelect={selectNode}
              isRoot
            />
          </ul>
        )}

        {conflicts.length > 0 && (
          <div className="mt-2 border-t border-amber/30 bg-amber/5 px-4 py-3">
            <div className="mb-2 flex items-center gap-2">
              <span className="text-amber">⚠︎</span>
              <span className="text-sm font-medium text-slate-200">
                {conflicts.length} already {conflicts.length === 1 ? "exists" : "exist"} here
              </span>
              {conflicts.length > 1 && (
                <div className="ml-auto flex gap-1">
                  <ApplyAllChip
                    label="Rename all"
                    active={allSame === "rename"}
                    onClick={() => setAllConflicts("rename")}
                  />
                  <ApplyAllChip
                    label="Overwrite all"
                    active={allSame === "overwrite"}
                    onClick={() => setAllConflicts("overwrite")}
                  />
                  <ApplyAllChip
                    label="Skip all"
                    active={allSame === "skip"}
                    onClick={() => setAllConflicts("skip")}
                  />
                </div>
              )}
            </div>
            <ul className="space-y-1">
              {conflicts.map((n) => (
                <li key={n} className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate font-mono text-[13px] text-slate-300">
                    {n}
                  </span>
                  <ConflictPick
                    value={resolution[n] ?? "rename"}
                    onChange={(m) =>
                      setResolution((p) => ({ ...p, [n]: m }))
                    }
                  />
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>

      {/* Bottom: upload bar. */}
      <div className="shrink-0 border-t border-edge bg-panel p-3 pb-safe">
        <div className="mb-2 truncate text-xs text-slate-500">
          Uploading to{" "}
          <span className="font-mono text-slate-300">
            {selectedDir ? rel(selectedDir) || basename(root || "") : "…"}
          </span>
          {skipCount > 0 && (
            <span className="ml-2 text-amber">({skipCount} skipped)</span>
          )}
        </div>
        {uploading ? (
          <div className="flex items-center gap-3">
            <div className="h-2 flex-1 overflow-hidden rounded-full bg-bar">
              <div
                className="h-full bg-accent transition-[width] duration-150"
                style={{ width: `${Math.round(progress * 100)}%` }}
              />
            </div>
            <span className="w-12 text-right text-xs tabular-nums text-slate-400">
              {Math.round(progress * 100)}%
            </span>
          </div>
        ) : (
          <div className="flex gap-2">
            <button
              onClick={close}
              className="rounded-lg bg-bar px-4 py-3 text-sm text-slate-400 active:bg-edge"
            >
              Cancel
            </button>
            <button
              onClick={upload}
              disabled={!selectedDir || sendCount === 0}
              className="flex-1 rounded-lg bg-accent px-4 py-3 text-sm font-semibold text-bar active:opacity-80 disabled:opacity-40"
            >
              Upload {sendCount} {sendCount === 1 ? "file" : "files"} here
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// --- tree row ---

interface FolderRowProps {
  path: string;
  name: string;
  depth: number;
  cache: Map<string, DirEntry[]>;
  expanded: Set<string>;
  loading: Set<string>;
  selectedDir: string | null;
  onToggle: (path: string) => void;
  onSelect: (path: string) => void;
  isRoot?: boolean;
}

// FolderRow renders one folder + (if expanded) its children recursively.
//
// Two click targets per row, deliberately separated:
//   - chevron column: toggles expansion (peek inside without committing)
//   - body of the row: picks this folder as the upload destination, and
//     also expands as a side-effect (the obvious "show me what I just
//     picked" gesture).
//
// Visual: the selected row gets an accent left-edge stripe + tinted
// background, so the destination is always findable even far down a
// deep tree. Indentation is 16px per level, matching the shared dir tree —
// nesting reads at a glance without dominating the row.
function FolderRow({
  path,
  name,
  depth,
  cache,
  expanded,
  loading,
  selectedDir,
  onToggle,
  onSelect,
  isRoot,
}: FolderRowProps) {
  const isOpen = expanded.has(path);
  const isLoading = loading.has(path);
  const children = cache.get(path);
  const selected = selectedDir === path;
  // Unknown until the first load completes — "▶" still works as
  // affordance even when childPaths turn out to be empty; the row just
  // expands to show "(empty)" below. Simpler than tracking a third state.
  const couldExpand = !children || children.length > 0;
  // Indent each level. The root row sits flush so it doesn't waste
  // horizontal space before any nesting has happened.
  const pad = { paddingLeft: `${depth * 16 + 8}px` };

  return (
    <li>
      <div
        className={`group relative flex items-stretch ${
          selected ? "bg-accent/10" : "hover:bg-panel/60"
        }`}
      >
        {/* Selection stripe — full-row accent on the left edge. Drawn as
            its own absolute layer so the row's padding (which contains
            the indent) doesn't push it inward. */}
        <span
          aria-hidden
          className={`absolute inset-y-0 left-0 w-[3px] ${
            selected ? "bg-accent" : "bg-transparent"
          }`}
        />
        {/* Chevron — separate click target. opacity-0 (instead of hidden)
            keeps the grid aligned across rows with and without children. */}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggle(path);
          }}
          disabled={!couldExpand}
          tabIndex={-1}
          aria-label={isOpen ? "Collapse" : "Expand"}
          className={`flex items-center justify-center pl-1 pr-1 text-[10px] text-slate-500 transition-transform ${
            couldExpand ? "hover:text-slate-200" : "opacity-30"
          } ${isOpen ? "rotate-90" : ""}`}
          style={{ paddingLeft: `${depth * 16 + 4}px` }}
        >
          ▶
        </button>
        {/* Row body — selects this folder as destination. Indent matches
            the chevron so the icon column lines up across siblings. */}
        <button
          onClick={() => onSelect(path)}
          className="flex min-w-0 flex-1 items-center gap-2 py-1.5 pr-3 text-left"
          style={pad}
        >
          <span className="shrink-0 text-slate-500">📁</span>
          <span
            className={`min-w-0 flex-1 truncate font-mono text-[13px] ${
              selected ? "text-slate-100" : "text-slate-300"
            }`}
          >
            {isRoot && depth === 0 ? `${name} (project root)` : name}
          </span>
          {isLoading && (
            <span className="shrink-0 text-xs text-slate-500">…</span>
          )}
          {selected && (
            <span
              aria-hidden
              className="shrink-0 rounded bg-accent px-1.5 py-0.5 text-[9px] font-bold uppercase text-bar"
            >
              here
            </span>
          )}
        </button>
      </div>

      {isOpen && children && children.length === 0 && !isLoading && (
        <div
          className="text-xs text-slate-600"
          style={{ paddingLeft: `${(depth + 1) * 16 + 24}px`, paddingBlock: "4px" }}
        >
          (empty)
        </div>
      )}

      {isOpen && children && children.length > 0 && (
        <ul>
          {children.map((c) => (
            <FolderRow
              key={c.path}
              path={c.path}
              name={c.name}
              depth={depth + 1}
              cache={cache}
              expanded={expanded}
              loading={loading}
              selectedDir={selectedDir}
              onToggle={onToggle}
              onSelect={onSelect}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

// --- small components ---

// ConflictPick — three-chip per-row mode picker. Compact enough to sit
// next to a moderate filename; the file column truncates.
function ConflictPick({
  value,
  onChange,
}: {
  value: UploadMode;
  onChange: (m: UploadMode) => void;
}) {
  const opts: { v: UploadMode; label: string }[] = [
    { v: "rename", label: "Rename" },
    { v: "overwrite", label: "Overwrite" },
    { v: "skip", label: "Skip" },
  ];
  return (
    <div className="flex shrink-0 overflow-hidden rounded-md border border-edge">
      {opts.map((o, i) => (
        <button
          key={o.v}
          onClick={() => onChange(o.v)}
          className={`px-2 py-1 text-[11px] ${
            i > 0 ? "border-l border-edge" : ""
          } ${
            value === o.v
              ? "bg-accent text-bar"
              : "bg-bar text-slate-400 active:bg-edge"
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function ApplyAllChip({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`rounded-md px-2 py-1 text-[11px] ${
        active ? "bg-accent text-bar" : "bg-bar text-slate-400 active:bg-edge"
      }`}
    >
      {label}
    </button>
  );
}
