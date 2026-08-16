// The markdown **reading view**, shared by the editor overlay and the
// read-only link-grant page (`/s/<token>`, proposal 0083 Part C). Extracted
// from EditorOverlay so LinkView can render prose without lazy-loading the
// whole editor (CodeMirror, the tree, the agent mirror) behind it.
//
// ── RENDERING INVARIANT (a security boundary — keep it through refactors) ────
// This module renders **untrusted** content. On the `/s/` page the reader has
// no account and the author is whoever the sharer's assistant wrote the file
// as; the page is served from the product's own origin, which makes it a
// phishing canvas if it can be made to execute anything.
//
// The boundary is react-markdown's defaults: raw HTML is NOT rendered, and
// `javascript:` / `data:` hrefs are stripped by its default URL transform. So:
//
//   * never add `rehype-raw` (or any HTML-passthrough plugin) here;
//   * never use `dangerouslySetInnerHTML` here;
//   * never let a component override interpolate content into a URL/attribute
//     that the browser would execute.
//
// The asset layer's CSP (`script-src 'self'`, crates/auth/src/headers.rs) is
// defense in depth for this, not the primary control. Loosening any of the
// above reopens [0042] for the whole product, not just one file.

import {
  Children,
  isValidElement,
  useCallback,
  useMemo,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { writeClipboard } from "../util";
import { noteUserCopy } from "../osc52Bus";

// CodeBlock overrides react-markdown's <pre> for fenced (```) code blocks,
// floating a copy button over it. We read the rendered text off the <pre>
// via a ref (innerText) instead of walking the markdown AST, so it copies
// exactly what's shown regardless of nested syntax nodes. writeClipboard
// handles the HTTPS (async clipboard) vs plain-HTTP (execCommand) split, so
// copying works on the tailnet's http:// deployment too. Inline `code` is
// untouched — only fenced blocks render through <pre>.
export function CodeBlock({ children }: { children?: ReactNode }) {
  const ref = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);
  const onCopy = useCallback(() => {
    const text = ref.current?.innerText ?? "";
    if (!text) return;
    noteUserCopy(); // 0077 A10: don't let a session's OSC 52 swap this out
    writeClipboard(text)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => {});
  }, []);
  return (
    <div className="cc-codeblock">
      <button type="button" className="cc-copy-btn" onClick={onCopy} aria-label="Copy code">
        {copied ? "Copied" : "Copy"}
      </button>
      <pre ref={ref}>{children}</pre>
    </div>
  );
}

// TaskCheckbox is the enabled, styled checkbox rendered in reading mode in place
// of react-markdown's disabled task-list input. It's purely presentational + one
// callback; the actual source rewrite + save happens in the parent. `preventDefault`
// on press stops a tap from focus-stealing / scroll-jumping on the phone PWA (the
// [0009] lesson); the toggle runs on click so keyboard (Enter/Space) works too.
function TaskCheckbox({ checked, onToggle }: { checked: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      aria-label={checked ? "Mark task incomplete" : "Mark task complete"}
      className={"cc-task-checkbox" + (checked ? " is-checked" : "")}
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onToggle();
      }}
    />
  );
}

// makeMarkdownComponents builds the react-markdown component overrides for the
// reading view: the fenced-code copy button (`pre`) plus a task-list `li` that
// swaps react-markdown's disabled checkbox for an enabled, clickable one. The
// `<li>` carries the source position via remark's `node.position` (the input
// itself has none), so toggling is anchored to the exact line — robust to
// duplicate text and nesting (Part A). Built per-`onToggleTask` so the handler
// stays current.
//
// `onToggleTask` is optional: the `/s/` page passes none, so checkboxes render
// INERT there. That is not a disabled-looking control with a live handler
// behind it — [0030]'s write path is simply absent from that page.
export function makeMarkdownComponents(onToggleTask?: (sourceOffset: number) => void) {
  return {
    pre: CodeBlock,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    li: (props: any) => {
      const cls: unknown = props.node?.properties?.className;
      const isTask = Array.isArray(cls) && cls.includes("task-list-item");
      const offset: unknown = props.node?.position?.start?.offset;
      if (isTask && typeof offset === "number") {
        const kids = Children.toArray(props.children);
        const idx = kids.findIndex(
          (k) => isValidElement(k) && (k as ReactElement<{ type?: string }>).props.type === "checkbox"
        );
        if (idx >= 0) {
          const checked = !!(kids[idx] as ReactElement<{ checked?: boolean }>).props.checked;
          const rest = kids.filter((_, i) => i !== idx);
          return (
            <li className={"task-list-item" + (checked ? " cc-task-done" : "")}>
              {onToggleTask ? (
                <TaskCheckbox checked={checked} onToggle={() => onToggleTask(offset)} />
              ) : (
                <span className={"cc-task-checkbox" + (checked ? " is-checked" : "")} aria-hidden="true" />
              )}
              {rest}
            </li>
          );
        }
      }
      return <li>{props.children}</li>;
    },
  };
}

// ReadingView renders the markdown fully (Obsidian's "reading mode"). It shares
// the writing surface's centered measure so toggling Edit<->Read doesn't shift
// the text column. `onToggleTask` flips a task-list checkbox at a source offset;
// omit it for a read-only surface.
export default function ReadingView({
  content,
  onToggleTask,
}: {
  content: string;
  onToggleTask?: (sourceOffset: number) => void;
}) {
  const components = useMemo(() => makeMarkdownComponents(onToggleTask), [onToggleTask]);
  return (
    <div className="h-full overflow-y-auto px-6 py-10">
      <div className="cc-prose mx-auto" style={{ maxWidth: "var(--cc-measure, 44rem)" }}>
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
          {content}
        </ReactMarkdown>
      </div>
    </div>
  );
}
