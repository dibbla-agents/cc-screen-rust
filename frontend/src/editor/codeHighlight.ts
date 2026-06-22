// Syntax highlighting for non-markdown code files in the file editor.
//
// The markdown path has had a hand-tuned highlight style (`mdHighlight` in
// livePreview.ts) since day one; code files, by contrast, ran the Lezer parser
// but mapped NO tags to colors, so they fell back to CodeMirror's bundled
// default style — which is tuned for a light background and renders washed-out,
// near-monochrome tokens on this app's dark editor. This module supplies the
// missing style: a VS Code "Dark+"-inspired palette, expressed via `--cc-syn-*`
// CSS variables (defined in index.css) so a future light mode is a token swap,
// not a second highlight definition. Mirrors how `mdHighlight` is built and
// applied via `syntaxHighlighting(...)`.

import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import type { Extension } from "@codemirror/state";

// Anything not listed here inherits the editor's base ink (#e2e8f0 from
// codeTheme) — a legible default, never invisible.
const codeHighlight = HighlightStyle.define([
  {
    tag: [t.keyword, t.modifier, t.controlKeyword, t.operatorKeyword, t.moduleKeyword],
    color: "var(--cc-syn-keyword)",
  },
  { tag: [t.string, t.special(t.string), t.regexp], color: "var(--cc-syn-string)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--cc-syn-number)" },
  {
    tag: [t.comment, t.lineComment, t.blockComment, t.docComment],
    color: "var(--cc-syn-comment)",
    fontStyle: "italic",
  },
  {
    tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName],
    color: "var(--cc-syn-function)",
  },
  {
    tag: [t.typeName, t.className, t.namespace, t.definition(t.typeName)],
    color: "var(--cc-syn-type)",
  },
  { tag: [t.variableName, t.propertyName, t.attributeName], color: "var(--cc-syn-variable)" },
  { tag: [t.definition(t.variableName), t.definition(t.propertyName)], color: "var(--cc-syn-def)" },
  { tag: [t.operator, t.punctuation, t.separator, t.bracket], color: "var(--cc-syn-punct)" },
  { tag: [t.meta, t.processingInstruction], color: "var(--cc-syn-meta)" },
  { tag: t.tagName, color: "var(--cc-syn-tag)" },
  { tag: t.invalid, color: "var(--cc-syn-invalid)" },
  { tag: [t.heading], color: "var(--cc-syn-keyword)", fontWeight: "600" },
  { tag: [t.strong], fontWeight: "600" },
  { tag: [t.emphasis], fontStyle: "italic" },
  { tag: [t.link, t.url], color: "var(--cc-syn-string)", textDecoration: "underline" },
]);

// The extension to push on the code branch of the editor (beside the detected
// language). Non-`fallback`, so it wins over basicSetup's defaultHighlightStyle.
export function codeHighlightExtension(): Extension {
  return syntaxHighlighting(codeHighlight);
}

// Exported for unit testing the tag→color mapping in isolation.
export { codeHighlight };
