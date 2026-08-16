// Proposal 0083 Part C — `/s/<token>`, the read-only link grant page.
//
// The reader has no account and gets nothing but this one file: no tree, no
// session UI, no app chrome, no way to write. It renders BEFORE App's auth
// gate (App.tsx), so nothing about the product is mounted behind it.
//
// Three things here are deliberate and load-bearing:
//
//   1. Markdown goes through the SHARED reading view (`./ReadingView`), whose
//      rendering invariant — no raw HTML, no `dangerouslySetInnerHTML`, no
//      HTML-passthrough plugin — is what makes untrusted prose safe to render
//      on the product's own origin. Code goes through CodeMirror in read-only
//      mode, which renders text nodes and highlights them; it never injects
//      markup either.
//   2. Task checkboxes render INERT: `ReadingView` is given no `onToggleTask`,
//      so [0030]'s write path is absent rather than disabled.
//   3. The provenance banner is fixed and the sharer cannot influence it. A
//      rendered page on app.ccscreen.dev is otherwise a free phishing canvas.

import { lazy, Suspense, useEffect, useState } from "react";
import { ApiError, getLinkContent, getLinkMeta, type LinkMeta } from "../api";
import ReadingView from "./ReadingView";

// CodeMirror is heavy; a markdown link (the common case) must not pay for it.
const MarkdownEditor = lazy(() => import("./MarkdownEditor"));

type State =
  | { t: "loading" }
  | { t: "ready"; content: string }
  | { t: "nopreview" }
  | { t: "offline" }
  | { t: "busy" }
  | { t: "gone" };

export default function LinkView({ token }: { token: string }) {
  const [meta, setMeta] = useState<LinkMeta | null>(null);
  const [state, setState] = useState<State>({ t: "loading" });

  useEffect(() => {
    let cancelled = false;
    getLinkMeta(token)
      .then((m) => {
        if (cancelled) return;
        setMeta(m);
        document.title = `${m.name} — cc-screen`;
        return getLinkContent(token).then((c) => {
          if (!cancelled) setState({ t: "ready", content: c });
        });
      })
      .catch((e) => {
        if (cancelled) return;
        const status = e instanceof ApiError ? e.status : 0;
        // 404 is the single undifferentiated refusal (bad/unknown/revoked/
        // expired/gone) — the page must not try to tell those apart either.
        if (status === 415) setState({ t: "nopreview" });
        else if (status === 503) setState({ t: "offline" });
        else if (status === 429) setState({ t: "busy" });
        else setState({ t: "gone" });
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  const isMd = !!meta && meta.mimeClass === "markdown";

  return (
    <div className="fixed inset-0 flex flex-col bg-bar text-slate-200">
      {/* Header: the filename and where it came from. No actions — there are
          none to offer. */}
      <header className="flex items-baseline gap-2 border-b border-edge bg-bar/95 px-4 py-3 pt-safe">
        <span className="min-w-0 flex-1 truncate text-sm font-semibold tracking-tight text-slate-100">
          {meta?.name ?? "Shared file"}
        </span>
        <span className="shrink-0 rounded border border-edge px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-slate-500">
          read-only
        </span>
      </header>

      <main className="min-h-0 flex-1 overflow-hidden" style={{ "--cc-editor-font": "14px" } as React.CSSProperties}>
        {state.t === "loading" && (
          <div className="flex h-full items-center justify-center text-sm text-slate-500">Loading…</div>
        )}
        {state.t === "ready" &&
          (isMd ? (
            <ReadingView content={state.content} />
          ) : (
            <Suspense
              fallback={
                <div className="flex h-full items-center justify-center text-sm text-slate-500">Loading…</div>
              }
            >
              <MarkdownEditor
                value={state.content}
                onChange={() => {}}
                filename={meta?.name ?? "file.txt"}
                markdown={false}
                readOnly
              />
            </Suspense>
          ))}
        {state.t === "nopreview" && (
          <Message
            title="No preview for this file type"
            body="This link points at a file cc-screen can’t show as text."
          />
        )}
        {state.t === "offline" && (
          <Message
            title="That machine is offline"
            body="The computer holding this file isn’t reachable right now. The link still works — try again later."
            action={{ label: "Try again", onClick: () => window.location.reload() }}
          />
        )}
        {state.t === "busy" && (
          <Message title="Too many requests" body="This link is being read too often. Try again in a minute." />
        )}
        {state.t === "gone" && (
          <Message
            title="This link isn’t available"
            body="It may have been revoked, expired, or never existed."
          />
        )}
      </main>

      {/* Fixed provenance banner — not configurable, not dismissible, and never
          fed by the shared content. */}
      <footer className="border-t border-edge bg-bar/95 px-4 py-2 text-center text-[11px] text-slate-500 pb-safe">
        Read-only file shared via cc-screen — the content is the sharer’s, not ccscreen’s.
        {meta?.machine ? <span className="ml-1 text-slate-600">({meta.machine})</span> : null}
      </footer>
    </div>
  );
}

function Message({
  title,
  body,
  action,
}: {
  title: string;
  body: string;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center">
      <div className="text-sm font-semibold text-slate-300">{title}</div>
      <div className="max-w-sm text-sm text-slate-500">{body}</div>
      {action && (
        <button
          onClick={action.onClick}
          className="rounded-md border border-edge bg-panel px-3 py-1.5 text-xs text-slate-200 hover:bg-edge"
        >
          {action.label}
        </button>
      )}
    </div>
  );
}
