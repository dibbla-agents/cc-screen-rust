import { useEffect, useMemo, useRef, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView, keymap } from "@codemirror/view";
import { Prec, type Extension } from "@codemirror/state";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { markdownExtensions } from "../editor/markdown";
import { livePreview } from "../editor/livePreview";
import { codeHighlightExtension } from "../editor/codeHighlight";

interface Props {
  value: string;
  onChange: (v: string) => void;
  filename: string;
  // When true, render markdown with the Obsidian-style live preview. Off for
  // non-markdown files (we highlight them as code via language-data instead).
  markdown: boolean;
  onSave?: () => void;
}

// The editing surface comes in two flavours. Markdown = a centered editorial
// "writing surface": serif prose (Newsreader), a comfortable measure, warm ink
// and generous margins, so live-preview reads like a page. Code = a full-width
// monospace buffer with line numbers. Base font size is driven by the
// --cc-editor-font CSS variable the overlay sets, so the A−/A+ control resizes
// without reconfiguring CodeMirror (the live-preview heading ems scale with it).
const proseTheme = EditorView.theme(
  {
    "&": { backgroundColor: "transparent", color: "var(--cc-ink, #d7dade)", height: "100%" },
    ".cm-scroller": { lineHeight: "1.7", overflow: "auto" },
    ".cm-content": {
      fontFamily: "var(--cc-prose-font)",
      fontSize: "var(--cc-editor-font, 15px)",
      caretColor: "#38bdf8",
      // A centered reading measure with breathing room top & bottom — the page,
      // not the buffer. 12vh bottom padding keeps the last line off the footer.
      maxWidth: "var(--cc-measure, 44rem)",
      margin: "0 auto",
      padding: "2rem 1.5rem 12vh",
    },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "#38bdf8", borderLeftWidth: "2px" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      { backgroundColor: "rgba(56,189,248,0.18)" },
    "&.cm-editor.cm-focused": { outline: "none" },
  },
  { dark: true }
);

const codeTheme = EditorView.theme(
  {
    "&": { backgroundColor: "transparent", color: "#e2e8f0", height: "100%" },
    ".cm-content": {
      fontFamily: "var(--cc-mono-font)",
      fontSize: "var(--cc-editor-font, 14px)",
      caretColor: "#38bdf8",
      padding: "1rem 1.25rem 12vh",
    },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "#38bdf8" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      { backgroundColor: "#243042" },
    ".cm-gutters": { backgroundColor: "transparent", color: "#475569", border: "none" },
    ".cm-activeLine": { backgroundColor: "rgba(36,48,66,0.35)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent", color: "#64748b" },
    "&.cm-editor.cm-focused": { outline: "none" },
    ".cm-scroller": { lineHeight: "1.6", overflow: "auto" },
  },
  { dark: true }
);

// MarkdownEditor wraps CodeMirror 6. For markdown files it layers in the live
// preview; for other text files it lazy-loads a syntax-highlighting language by
// extension (language-data's descriptors load on demand). A Mod-s keymap (high
// precedence so it beats the browser's Save dialog) calls onSave.
export default function MarkdownEditor({ value, onChange, filename, markdown, onSave }: Props) {
  // Keep onSave in a ref so the Mod-s keymap doesn't force the whole extension
  // set (and the live-preview plugin) to rebuild on every keystroke — the
  // parent passes a fresh onSave each render.
  const onSaveRef = useRef(onSave);
  onSaveRef.current = onSave;
  // Lazily-loaded language support for non-markdown files.
  const [codeLang, setCodeLang] = useState<Extension | null>(null);
  useEffect(() => {
    if (markdown) {
      setCodeLang(null);
      return;
    }
    const desc = LanguageDescription.matchFilename(languages, filename);
    if (!desc) {
      setCodeLang(null);
      return;
    }
    let cancelled = false;
    desc
      .load()
      .then((sup) => {
        if (!cancelled) setCodeLang(sup);
      })
      .catch(() => {
        if (!cancelled) setCodeLang(null);
      });
    return () => {
      cancelled = true;
    };
  }, [markdown, filename]);

  const extensions = useMemo<Extension[]>(() => {
    const exts: Extension[] = [
      EditorView.lineWrapping,
      Prec.highest(
        keymap.of([
          {
            key: "Mod-s",
            preventDefault: true,
            run: () => {
              onSaveRef.current?.();
              return true;
            },
          },
        ])
      ),
    ];
    if (markdown) {
      exts.push(markdownExtensions(), livePreview());
    } else if (codeLang) {
      exts.push(codeLang);
      // The missing piece: map Lezer highlight tags to our dark palette. Added
      // after basicSetup's fallback default, so our (non-fallback) style wins.
      exts.push(codeHighlightExtension());
    }
    return exts;
  }, [markdown, codeLang]);

  return (
    <CodeMirror
      value={value}
      onChange={onChange}
      extensions={extensions}
      theme={markdown ? proseTheme : codeTheme}
      height="100%"
      style={{ height: "100%" }}
      basicSetup={{
        lineNumbers: !markdown,
        foldGutter: false,
        highlightActiveLine: !markdown,
        highlightActiveLineGutter: !markdown,
        bracketMatching: true,
        closeBrackets: false,
        autocompletion: false,
      }}
    />
  );
}
