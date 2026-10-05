import { useCallback, useState, type SyntheticEvent } from "react";
import { inlineURL, saveFileToDevice } from "../api";
import { DownloadIcon } from "../icons";

interface Props {
  // $HOME-relative path of the image (used for the inline byte stream + download).
  path: string;
  // Basename, for the alt text and the download filename.
  name: string;
  // The machine (agent) the file lives on, threaded onto the byte-stream URL so
  // a hub serves it from the right agent. "" for a single agent.
  machine?: string;
}

type Status = "loading" | "ready" | "error";

// ImageViewer shows a raster image read-only, fit to the pane. The browser does
// the decoding: the bytes come from the same inline stream the PDF viewer uses
// (/api/download?inline=1, already relayed by the hub and typed by the agent's
// mime_guess), so there is no server or protocol side to this.
//
// Zoom is a fit ↔ 100% toggle — tap the image or the pill — rather than pinch,
// because the PWA's viewport sets user-scalable=no. At 100% the pane scrolls.
// The parent keys this component by path, so state resets per file.
export default function ImageViewer({ path, name, machine }: Props) {
  const [status, setStatus] = useState<Status>("loading");
  const [fit, setFit] = useState(true);
  const [natural, setNatural] = useState<{ w: number; h: number } | null>(null);
  // Whether fit mode actually shrank the image — only then is 100% different.
  const [scaled, setScaled] = useState(false);
  const [downloading, setDownloading] = useState(false);

  const onLoad = useCallback((e: SyntheticEvent<HTMLImageElement>) => {
    const img = e.currentTarget;
    setNatural({ w: img.naturalWidth, h: img.naturalHeight });
    setScaled(img.naturalWidth > img.clientWidth || img.naturalHeight > img.clientHeight);
    setStatus("ready");
  }, []);

  const onDownload = useCallback(async () => {
    if (downloading) return;
    setDownloading(true);
    try {
      await saveFileToDevice(path, name, machine);
    } catch {
      // best-effort; the download path has its own browser fallback
    } finally {
      setDownloading(false);
    }
  }, [downloading, path, name, machine]);

  const toggle = () => {
    if (scaled || !fit) setFit((f) => !f);
  };

  return (
    <div className="relative h-full">
      <div className="h-full overflow-auto overscroll-contain">
        <div
          className={`flex min-h-full items-center justify-center p-3 ${fit ? "h-full" : "min-w-fit"}`}
        >
          <img
            src={inlineURL(path, machine)}
            alt={name}
            onLoad={onLoad}
            onError={() => setStatus("error")}
            onClick={toggle}
            draggable={false}
            className={`${status === "ready" ? "" : "invisible"} ${
              fit ? `max-h-full max-w-full object-contain ${scaled ? "cursor-zoom-in" : ""}` : "max-w-none cursor-zoom-out"
            }`}
          />
        </div>
      </div>

      {status === "loading" && (
        <div className="absolute inset-0 flex items-center justify-center text-sm text-slate-400">
          Loading image…
        </div>
      )}
      {status === "error" && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-8 text-center">
          <p className="text-sm text-red-400">Couldn’t display this image.</p>
          <button
            onClick={() => void onDownload()}
            className="flex items-center gap-1.5 rounded-lg bg-panel px-3 py-2 text-sm text-slate-200 ring-1 ring-inset ring-edge hover:bg-edge"
          >
            <DownloadIcon className="h-4 w-4" /> Download instead
          </button>
        </div>
      )}

      {/* Floating pill — dimensions · fit/100% — styled like the PDF viewer's bar. */}
      {status === "ready" && natural && (
        <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center px-3 pb-safe">
          <div className="pointer-events-auto flex items-center gap-1 rounded-full border border-edge bg-bar/95 px-1.5 py-1 text-slate-200 shadow-xl backdrop-blur">
            <span className="px-2 text-xs tabular-nums text-slate-400">
              {natural.w} × {natural.h}
            </span>
            {(scaled || !fit) && (
              <>
                <span className="mx-0.5 h-5 w-px bg-edge" aria-hidden="true" />
                <button
                  onClick={toggle}
                  className="rounded-md px-2 py-1 text-xs text-slate-300 hover:bg-panel active:bg-edge"
                  title={fit ? "Show at 100%" : "Fit to pane"}
                  aria-label={fit ? "Show image at 100%" : "Fit image to pane"}
                >
                  {fit ? "100%" : "Fit"}
                </button>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
