import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import type { PDFDocumentProxy, RenderTask } from "pdfjs-dist";
import { inlineURL, saveFileToDevice } from "../api";
import { DownloadIcon } from "../icons";

// The pdf.js worker is bundled by Vite as an .mjs asset and served by the Go
// static embed (which pins the .mjs MIME type — module workers need a JS
// content-type). It's loaded as a module worker.
pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

// This whole module — and pdf.js with it — is lazy-loaded by EditorOverlay
// (React.lazy), so opening a markdown/text file never pulls the PDF stack.

interface Props {
  // $HOME-relative path of the PDF (used for the inline byte stream + download).
  path: string;
  // Basename, for the download filename.
  name: string;
}

type Status = "loading" | "ready" | "error";

const ZOOM_MIN = 0.5;
const ZOOM_MAX = 4;
const ZOOM_STEP = 1.25;
// Horizontal breathing room subtracted from the scroll container's width to get
// the fit-to-width page size (matches the px-3 padding, both sides + a little).
const SIDE_PAD = 24;
// Cap the backing-store scale so a 3× phone display doesn't blow canvas memory
// on a long document; 2× is already crisp.
const MAX_DPR = 2;
const clampZoom = (z: number) => Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, z));

// PdfViewer renders a PDF as a continuous, fit-to-width scroll of canvas pages.
// Pages render lazily as they near the viewport (IntersectionObserver) so a long
// document doesn't rasterise every page up front — important on a phone. Zoom is
// a multiplier on the fit-to-width scale; pages re-render crisply at the new
// scale (not a CSS stretch). A floating bar carries the page indicator, zoom and
// a download button. Read-only — PDFs are never edited here.
export default function PdfViewer({ path, name }: Props) {
  const url = inlineURL(path);

  const containerRef = useRef<HTMLDivElement | null>(null);
  const docRef = useRef<PDFDocumentProxy | null>(null);
  // Page wrapper divs by 1-based page number (populated via ref callbacks).
  const pageElsRef = useRef<Map<number, HTMLDivElement>>(new Map());
  // In-flight render task per page, so a superseding render (zoom/resize) can
  // cancel the previous one before touching the same canvas.
  const renderTasksRef = useRef<Map<number, RenderTask>>(new Map());
  // The "render key" (`fitWidth:zoom`) each page was last rendered at, so a
  // width/zoom change re-renders the pages that are on screen.
  const renderedAtRef = useRef<Map<number, string>>(new Map());

  const [status, setStatus] = useState<Status>("loading");
  const [error, setError] = useState("");
  const [numPages, setNumPages] = useState(0);
  // height/width of page 1 — a placeholder aspect for not-yet-rendered pages so
  // the scrollbar is roughly right and the observer fires at sensible offsets.
  const [pageAspect, setPageAspect] = useState(0);
  const [fitWidth, setFitWidth] = useState(0);
  const [zoom, setZoom] = useState(1);
  const [current, setCurrent] = useState(1);
  const [visible, setVisible] = useState<Set<number>>(new Set());
  const [downloading, setDownloading] = useState(false);

  // Mirror live values into refs for the async render function.
  const zoomRef = useRef(zoom);
  zoomRef.current = zoom;
  const fitWidthRef = useRef(fitWidth);
  fitWidthRef.current = fitWidth;

  // Track the scroll container's content width (fit-to-width target), seeded on
  // mount and kept in sync via ResizeObserver. rAF-coalesced so a window drag
  // doesn't re-render every page on every intermediate width.
  useLayoutEffect(() => {
    const root = containerRef.current;
    if (!root) return;
    const apply = () => setFitWidth(Math.max(240, root.clientWidth - SIDE_PAD));
    apply();
    let raf = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(apply);
    });
    ro.observe(root);
    return () => {
      ro.disconnect();
      cancelAnimationFrame(raf);
    };
  }, []);

  // Load (and re-load on path change) the document.
  useEffect(() => {
    let cancelled = false;
    setStatus("loading");
    setError("");
    setNumPages(0);
    setPageAspect(0);
    setVisible(new Set());
    setCurrent(1);
    pageElsRef.current.clear();
    renderedAtRef.current.clear();
    renderTasksRef.current.forEach((t) => t.cancel());
    renderTasksRef.current.clear();

    const task = pdfjsLib.getDocument({ url });
    task.promise
      .then(async (doc) => {
        if (cancelled) return;
        docRef.current = doc;
        try {
          const p1 = await doc.getPage(1);
          const v = p1.getViewport({ scale: 1 });
          if (!cancelled) setPageAspect(v.height / v.width);
        } catch {
          // fall back to a default aspect below
        }
        if (cancelled) return;
        setNumPages(doc.numPages);
        setVisible(new Set([1]));
        setStatus("ready");
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to load PDF");
          setStatus("error");
        }
      });

    return () => {
      cancelled = true;
      // destroy() aborts the load and tears down the worker doc.
      void task.destroy();
      docRef.current = null;
    };
  }, [url]);

  // Cancel any straggling render tasks on unmount.
  useEffect(
    () => () => {
      renderTasksRef.current.forEach((t) => t.cancel());
      renderTasksRef.current.clear();
    },
    []
  );

  // Render one page into its canvas at the current fit-width × zoom (and DPR).
  const renderPage = useCallback(async (n: number, key: string) => {
    const doc = docRef.current;
    const wrap = pageElsRef.current.get(n);
    if (!doc || !wrap) return;
    const canvas = wrap.querySelector("canvas");
    if (!canvas) return;
    renderTasksRef.current.get(n)?.cancel();

    let page;
    try {
      page = await doc.getPage(n);
    } catch {
      return;
    }
    const unscaled = page.getViewport({ scale: 1 });
    const targetCssW = fitWidthRef.current * zoomRef.current;
    if (targetCssW <= 0) return;
    const dpr = Math.min(window.devicePixelRatio || 1, MAX_DPR);
    const viewport = page.getViewport({ scale: (targetCssW / unscaled.width) * dpr });
    canvas.width = Math.floor(viewport.width);
    canvas.height = Math.floor(viewport.height);
    canvas.style.width = `${Math.floor(viewport.width / dpr)}px`;
    canvas.style.height = `${Math.floor(viewport.height / dpr)}px`;

    const renderTask = page.render({ canvas, viewport });
    renderTasksRef.current.set(n, renderTask);
    try {
      await renderTask.promise;
      renderedAtRef.current.set(n, key);
    } catch {
      // RenderingCancelledException when a later render superseded this one.
    }
  }, []);

  // Render the on-screen pages whenever the visible set, fit-width or zoom
  // changes. A new render key (width/zoom) invalidates prior renders, so visible
  // pages re-rasterise at the new scale; off-screen pages re-render when they
  // next scroll into view.
  useEffect(() => {
    if (status !== "ready") return;
    const key = `${fitWidth}:${zoom}`;
    visible.forEach((n) => {
      if (renderedAtRef.current.get(n) !== key) void renderPage(n, key);
    });
  }, [visible, fitWidth, zoom, status, renderPage]);

  // Observe page wrappers and keep the visible set fresh. rootMargin prefetches
  // ~2 viewports ahead/behind so scrolling rarely shows a blank page.
  useEffect(() => {
    if (status !== "ready") return;
    const root = containerRef.current;
    if (!root) return;
    const io = new IntersectionObserver(
      (entries) => {
        setVisible((prev) => {
          const next = new Set(prev);
          let changed = false;
          for (const e of entries) {
            const n = Number((e.target as HTMLElement).dataset.page);
            if (!n) continue;
            if (e.isIntersecting && !next.has(n)) {
              next.add(n);
              changed = true;
            } else if (!e.isIntersecting && next.has(n)) {
              next.delete(n);
              changed = true;
            }
          }
          return changed ? next : prev;
        });
      },
      { root, rootMargin: "200% 0px" }
    );
    pageElsRef.current.forEach((el) => io.observe(el));
    return () => io.disconnect();
  }, [status, numPages]);

  // Current-page indicator: the page crossing the viewport's vertical middle.
  // rAF-throttled; the ascending order lets us stop at the first page below mid.
  const updateCurrent = useCallback(() => {
    const root = containerRef.current;
    if (!root) return;
    const rect = root.getBoundingClientRect();
    const mid = rect.top + rect.height / 2;
    let best = 1;
    for (let n = 1; n <= pageElsRef.current.size; n++) {
      const el = pageElsRef.current.get(n);
      if (!el) continue;
      if (el.getBoundingClientRect().top <= mid) best = n;
      else break;
    }
    setCurrent(best);
  }, []);

  useEffect(() => {
    if (status !== "ready") return;
    const root = containerRef.current;
    if (!root) return;
    let raf = 0;
    const onScroll = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(updateCurrent);
    };
    root.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      root.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(raf);
    };
  }, [status, updateCurrent]);

  const onDownload = useCallback(async () => {
    if (downloading) return;
    setDownloading(true);
    try {
      await saveFileToDevice(path, name);
    } catch {
      // best-effort; the download path has its own browser fallback
    } finally {
      setDownloading(false);
    }
  }, [downloading, path, name]);

  const pageCssW = fitWidth > 0 ? Math.round(fitWidth * zoom) : 0;
  const estHeight = pageCssW > 0 ? Math.round(pageCssW * (pageAspect || 1.3)) : undefined;

  return (
    <div className="relative h-full bg-[#1b1f25]">
      <div ref={containerRef} className="h-full overflow-auto overscroll-contain">
        <div className="flex min-h-full min-w-fit flex-col items-center px-3 py-3">
          {status === "ready" &&
            Array.from({ length: numPages }, (_, i) => i + 1).map((n) => (
              <div
                key={n}
                data-page={n}
                ref={(el) => {
                  if (el) pageElsRef.current.set(n, el);
                  else pageElsRef.current.delete(n);
                }}
                className="cc-pdf-page my-2 bg-white shadow-lg shadow-black/40 ring-1 ring-black/30"
                style={{ width: pageCssW || undefined, minHeight: estHeight }}
              >
                <canvas className="cc-pdf-canvas block h-auto w-full" />
              </div>
            ))}
        </div>
      </div>

      {status === "loading" && (
        <div className="absolute inset-0 flex items-center justify-center text-sm text-slate-400">
          Loading PDF…
        </div>
      )}
      {status === "error" && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-8 text-center">
          <p className="text-sm text-red-400">Couldn’t display this PDF.</p>
          {error && <p className="max-w-md text-xs text-slate-500">{error}</p>}
          <button
            onClick={() => void onDownload()}
            className="flex items-center gap-1.5 rounded-lg bg-panel px-3 py-2 text-sm text-slate-200 ring-1 ring-inset ring-edge hover:bg-edge"
          >
            <DownloadIcon className="h-4 w-4" /> Download instead
          </button>
        </div>
      )}

      {/* Floating control bar — page indicator · zoom · download. Sits over the
          scroll area (doesn't scroll with it), thumb-reachable at the bottom. */}
      {status === "ready" && (
        <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center px-3 pb-safe">
          <div className="pointer-events-auto flex items-center gap-1 rounded-full border border-edge bg-bar/95 px-1.5 py-1 text-slate-200 shadow-xl backdrop-blur">
            <span
              className="px-2 text-xs tabular-nums text-slate-400"
              aria-label={`Page ${current} of ${numPages}`}
            >
              {current} / {numPages}
            </span>
            <span className="mx-0.5 h-5 w-px bg-edge" aria-hidden="true" />
            <button
              onClick={() => setZoom((z) => clampZoom(z / ZOOM_STEP))}
              disabled={zoom <= ZOOM_MIN}
              className="flex h-8 w-8 items-center justify-center rounded-full text-lg text-slate-300 hover:bg-panel active:bg-edge disabled:opacity-30"
              title="Zoom out"
              aria-label="Zoom out"
            >
              −
            </button>
            <button
              onClick={() => setZoom(1)}
              className="min-w-[3.5ch] rounded-md px-1 text-center text-xs tabular-nums text-slate-300 hover:bg-panel active:bg-edge"
              title="Reset zoom (fit width)"
              aria-label="Reset zoom to fit width"
            >
              {Math.round(zoom * 100)}%
            </button>
            <button
              onClick={() => setZoom((z) => clampZoom(z * ZOOM_STEP))}
              disabled={zoom >= ZOOM_MAX}
              className="flex h-8 w-8 items-center justify-center rounded-full text-lg text-slate-300 hover:bg-panel active:bg-edge disabled:opacity-30"
              title="Zoom in"
              aria-label="Zoom in"
            >
              +
            </button>
            <span className="mx-0.5 h-5 w-px bg-edge" aria-hidden="true" />
            <button
              onClick={() => void onDownload()}
              disabled={downloading}
              className="flex h-8 w-8 items-center justify-center rounded-full text-slate-300 hover:bg-panel active:bg-edge disabled:opacity-40"
              title="Download / save PDF"
              aria-label="Download PDF"
            >
              <DownloadIcon className="h-[18px] w-[18px]" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
