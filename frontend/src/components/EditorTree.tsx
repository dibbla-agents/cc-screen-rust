import { useEffect, useState, type RefObject } from "react";
import { type FileEntry } from "../api";
import { FunnelIcon } from "../icons";
import {
  FolderChildren,
  canOpenInEditor,
  useFolderDrop,
  useTreeContextHandlers,
  type TreeCtxInfo,
  type TreeSection,
  type useDirTree,
} from "./dirTree";

interface Props {
  tree: ReturnType<typeof useDirTree>;
  onOpenFile: (path: string) => void;
  // Save a file to the device. Backs the per-row download button and is the
  // tap-action for files that can't be opened in the editor.
  onDownload: (f: FileEntry) => void;
  downloadingPath?: string | null;
  // Right-click / long-press on a row or section header → open the CRUD menu.
  onContextMenu?: (
    e: { clientX: number; clientY: number; preventDefault: () => void },
    info: TreeCtxInfo
  ) => void;
  // OS-file drop → upload into that folder. Wired on both nested folder rows
  // (via FolderChildren) and the section headers (project / home / share), so
  // you can drop straight onto a section root too.
  onDropFiles?: (dir: string, dt: DataTransfer) => void;
  // Drag a node onto a folder / section root → move it there (proposal 0012).
  onMoveNode?: (src: string, destDir: string) => void;
  activePath: string | null;
  // Phone-sized rows (larger type, taller targets). Off = the tight desktop
  // sidebar. See FolderChildren's `touch`.
  touch?: boolean;
  // Type-to-filter (proposal 0038, Part C): hand the matching query over to
  // [0027]'s project-wide name search when the in-tree (loaded-only) filter
  // isn't enough — the "Search all files →" handoff.
  onSearchAll?: (query: string) => void;
  // Focus target for the tree-filter field (Ctrl+B / focuses it).
  filterInputRef?: RefObject<HTMLInputElement | null>;
}

// EditorTree — the editor's file tree (desktop left sidebar + phone slide-over).
// Presentational: it renders the share / project / home sections from a
// useDirTree instance the EditorOverlay owns (so the overlay can also use it for
// new-file anchoring). Tapping an editable text file or a PDF opens it (the
// overlay routes PDFs to its read-only pdf.js viewer); tapping anything else
// downloads it, and every row carries a download button so even openable files
// can be saved to the device. This is the single file view.
export default function EditorTree({ tree, onOpenFile, onDownload, downloadingPath, onContextMenu, onDropFiles, onMoveNode, activePath, touch = false, onSearchAll, filterInputRef }: Props) {
  const { cache, effectiveExpanded, loading, errs, toggle, sections, filterQuery, setFilterQuery, filter } = tree;
  const expanded = effectiveExpanded;
  // Section headers (Share / Project / Home) get the same right-click/long-press
  // menu as rows; FolderChildren runs its own copy for the nested rows.
  const { ctxHandlers, swallowLongPress } = useTreeContextHandlers(onContextMenu);

  const onFile = (f: FileEntry) => {
    if (canOpenInEditor(f.name)) onOpenFile(f.path);
    else onDownload(f);
  };

  // ── Filter-mode keyboard cursor (proposal 0038, Part C) ───────────────────
  // The filtered rows are a flat, render-ordered list (tree.filter.rows). The
  // cursor moves through them with ↑/↓ from the filter field; Enter opens a file
  // or toggles a folder — the keyboard nav the tree lacked entirely before.
  const rows = filter?.rows ?? [];
  const [cursor, setCursor] = useState(0);
  useEffect(() => {
    setCursor((c) => (c >= rows.length ? 0 : c));
  }, [rows.length]);
  useEffect(() => {
    setCursor(0);
  }, [filter?.query]);
  const cursorPath = filter && rows[cursor] ? rows[cursor].path : null;

  const onFilterKey = (e: React.KeyboardEvent) => {
    if (!filter || rows.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((c) => Math.min(c + 1, rows.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => Math.max(0, c - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const row = rows[cursor];
      if (!row) return;
      if (row.isDir) toggle(row.path);
      else onFile({ name: row.name, path: row.path, size: 0, mtime: 0 });
    }
  };

  // Section (Share / Project / Home) header rows scale with the same touch flag
  // as their children, so the whole tree reads at one size.
  const secCls = touch
    ? "flex w-full items-center gap-2 rounded-md px-1.5 py-2 text-left text-[15px] font-medium text-slate-200 active:bg-edge/40"
    : "flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-left text-[13px] font-medium text-slate-200 hover:bg-edge/40";

  const filterActiveEmpty = filter && rows.length === 0;

  return (
    // select-none + -webkit-touch-callout:none stop iOS Safari's long-press from
    // selecting the row text (blue highlight) and popping its native Copy / Look
    // Up callout over our context menu. A file tree never needs selectable text.
    <div className="flex h-full flex-col border-r border-edge bg-bar select-none [-webkit-touch-callout:none]">
      {/* ── Type-to-filter field (0038) — funnel glyph + "Filter tree", visually
          distinct from [0027]'s 🔎 "Find file…" bar so the two read as two. ── */}
      <div className={`flex shrink-0 items-center gap-1.5 border-b border-edge/60 px-2 ${touch ? "py-2" : "py-1.5"}`}>
        <FunnelIcon className={`shrink-0 text-slate-500 ${touch ? "h-4 w-4" : "h-3.5 w-3.5"}`} />
        <input
          ref={filterInputRef}
          value={filterQuery}
          onChange={(e) => setFilterQuery(e.target.value)}
          onKeyDown={onFilterKey}
          placeholder="Filter tree"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          aria-label="Filter the file tree"
          className={`min-w-0 flex-1 bg-transparent text-slate-100 placeholder:text-slate-600 outline-none ${touch ? "text-[15px]" : "text-[13px]"}`}
        />
        {filter && (
          <span className="shrink-0 rounded bg-panel/70 px-1.5 py-0.5 text-[10px] tabular-nums text-slate-400" title="Rows shown">
            {rows.length} shown
          </span>
        )}
        {filterQuery && (
          <button
            onClick={() => setFilterQuery("")}
            title="Clear filter (Esc)"
            aria-label="Clear tree filter"
            className="shrink-0 rounded px-1 text-slate-500 hover:text-slate-200"
          >
            ✕
          </button>
        )}
      </div>

      {/* The tiny caption is redundant on a phone (the panel header already says
          "Files"), so it's desktop-only. */}
      {!touch && (
        <div className="shrink-0 px-3 pb-1 pt-3 text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-600">
          Files
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-3">
        {sections.length === 0 && (
          <div className="px-2 py-6 text-center text-xs text-slate-600">Loading…</div>
        )}
        {filterActiveEmpty && (
          <div className="px-2 py-6 text-center text-xs text-slate-600">
            No loaded files match “{filter!.query}”.
          </div>
        )}
        {sections.map((sec) => {
          const path = sec.path;
          const isOpen = path ? expanded.has(path) : false;
          // Hide a whole section while filtering if it contains no visible rows.
          if (filter && path && !filter.visibleDirs.has(path) && !sectionHasMatch(sec, filter)) {
            return null;
          }
          const isLoading =
            (path && loading.has(path)) ||
            (sec.bySession ? loading.has(`session:${sec.bySession}`) : false);
          return (
            <div key={sec.key} className="mb-0.5">
              <SectionHeader
                sec={sec}
                isOpen={isOpen}
                isLoading={!!isLoading}
                className={secCls}
                ctxProps={path ? ctxHandlers({ path, name: sec.label, isDir: true }) : {}}
                onClick={() => {
                  if (swallowLongPress()) return;
                  toggle(path, { sectionErrKey: sec.key, bySession: sec.bySession });
                }}
                onDropFiles={onDropFiles}
                onMoveNode={onMoveNode}
              />
              {errs[sec.key] && (
                <div className="px-2 py-1 text-xs text-red-400">{errs[sec.key]}</div>
              )}
              {isOpen && path && (
                <FolderChildren
                  path={path}
                  depth={0}
                  cache={cache}
                  expanded={expanded}
                  loading={loading}
                  onToggle={(p) => toggle(p)}
                  onFile={onFile}
                  onDownload={onDownload}
                  downloadingPath={downloadingPath}
                  onContextMenu={onContextMenu}
                  onDropFiles={onDropFiles}
                  onMoveNode={onMoveNode}
                  activePath={activePath}
                  compact
                  touch={touch}
                  filter={filter}
                  cursorPath={cursorPath}
                />
              )}
            </div>
          );
        })}

        {/* Lazy-load caveat (0038): the in-tree filter only sees loaded nodes.
            Hand the query to [0027]'s project-wide name search — one tap, query
            carried over — so the two searches are two depths of one gesture. */}
        {filter && onSearchAll && (
          <button
            onClick={() => onSearchAll(filter.query)}
            className="mt-1 flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-left text-[12px] text-accent hover:bg-edge/40"
          >
            Search all files
            <span aria-hidden="true">→</span>
          </button>
        )}
      </div>
    </div>
  );
}

// A section is kept while filtering if its root is an ancestor of a match
// (visibleDirs) or any matched node lives under it. The flat row list already
// excludes empties, but section roots are headers (not in visibleDirs unless an
// ancestor), so check whether any matched path sits under this root.
function sectionHasMatch(sec: TreeSection, filter: NonNullable<ReturnType<typeof useDirTree>["filter"]>): boolean {
  if (!sec.path) return false;
  const prefix = sec.path.endsWith("/") ? sec.path : sec.path + "/";
  for (const p of filter.matchedFiles) if (p === sec.path || p.startsWith(prefix)) return true;
  for (const p of filter.matchedDirs) if (p === sec.path || p.startsWith(prefix)) return true;
  return false;
}

// SectionHeader — one Share / Project / Home root row. Split out so it can own
// a useFolderDrop instance (a hook can't be called inside the sections.map),
// making the section root itself a file drop target alongside its nested
// folders.
function SectionHeader({
  sec,
  isOpen,
  isLoading,
  className,
  ctxProps,
  onClick,
  onDropFiles,
  onMoveNode,
}: {
  sec: TreeSection;
  isOpen: boolean;
  isLoading: boolean;
  className: string;
  ctxProps: ReturnType<ReturnType<typeof useTreeContextHandlers>["ctxHandlers"]>;
  onClick: () => void;
  onDropFiles?: (dir: string, dt: DataTransfer) => void;
  onMoveNode?: (src: string, destDir: string) => void;
}) {
  const { over, dropHandlers } = useFolderDrop(sec.path || null, onDropFiles, onMoveNode);
  return (
    <button
      onClick={onClick}
      className={`${className} ${over ? "bg-accent/15 ring-1 ring-inset ring-accent/60" : ""}`}
      {...ctxProps}
      {...dropHandlers}
    >
      <svg
        viewBox="0 0 24 24"
        width="11"
        height="11"
        className={`shrink-0 text-slate-500 transition-transform duration-150 ${isOpen ? "rotate-90" : ""}`}
        aria-hidden="true"
      >
        <path d="M9 6l6 6-6 6" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
      <span className="min-w-0 flex-1 truncate">{sec.label}</span>
      {isLoading && <span className="text-xs text-slate-600">…</span>}
    </button>
  );
}
