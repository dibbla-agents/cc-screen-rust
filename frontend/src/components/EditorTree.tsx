import { type FileEntry } from "../api";
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
  activePath: string | null;
  // Phone-sized rows (larger type, taller targets). Off = the tight desktop
  // sidebar. See FolderChildren's `touch`.
  touch?: boolean;
}

// EditorTree — the editor's file tree (desktop left sidebar + phone slide-over).
// Presentational: it renders the share / project / home sections from a
// useDirTree instance the EditorOverlay owns (so the overlay can also use it for
// new-file anchoring). Tapping an editable text file or a PDF opens it (the
// overlay routes PDFs to its read-only pdf.js viewer); tapping anything else
// downloads it, and every row carries a download button so even openable files
// can be saved to the device. This is the single file view.
export default function EditorTree({ tree, onOpenFile, onDownload, downloadingPath, onContextMenu, onDropFiles, activePath, touch = false }: Props) {
  const { cache, expanded, loading, errs, toggle, sections } = tree;
  // Section headers (Share / Project / Home) get the same right-click/long-press
  // menu as rows; FolderChildren runs its own copy for the nested rows.
  const { ctxHandlers, swallowLongPress } = useTreeContextHandlers(onContextMenu);

  const onFile = (f: FileEntry) => {
    if (canOpenInEditor(f.name)) onOpenFile(f.path);
    else onDownload(f);
  };

  // Section (Share / Project / Home) header rows scale with the same touch flag
  // as their children, so the whole tree reads at one size.
  const secCls = touch
    ? "flex w-full items-center gap-2 rounded-md px-1.5 py-2 text-left text-[15px] font-medium text-slate-200 active:bg-edge/40"
    : "flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-left text-[13px] font-medium text-slate-200 hover:bg-edge/40";

  return (
    // select-none + -webkit-touch-callout:none stop iOS Safari's long-press from
    // selecting the row text (blue highlight) and popping its native Copy / Look
    // Up callout over our context menu. A file tree never needs selectable text.
    <div className="flex h-full flex-col border-r border-edge bg-bar select-none [-webkit-touch-callout:none]">
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
        {sections.map((sec) => {
          const path = sec.path;
          const isOpen = path ? expanded.has(path) : false;
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
                  activePath={activePath}
                  compact
                  touch={touch}
                />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
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
}: {
  sec: TreeSection;
  isOpen: boolean;
  isLoading: boolean;
  className: string;
  ctxProps: ReturnType<ReturnType<typeof useTreeContextHandlers>["ctxHandlers"]>;
  onClick: () => void;
  onDropFiles?: (dir: string, dt: DataTransfer) => void;
}) {
  const { over, dropHandlers } = useFolderDrop(sec.path || null, onDropFiles);
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
