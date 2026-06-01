import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { languages } from "@codemirror/language-data";
import type { Extension } from "@codemirror/state";

// markdownExtensions is the shared CodeMirror language setup for the editor.
// `markdownLanguage` already enables GFM (strikethrough, tables, task lists);
// `codeLanguages: languages` highlights fenced code blocks by their info string
// (```js, ```python, …) and is also what will drive future code-file editing.
export function markdownExtensions(): Extension {
  return markdown({ base: markdownLanguage, codeLanguages: languages });
}
