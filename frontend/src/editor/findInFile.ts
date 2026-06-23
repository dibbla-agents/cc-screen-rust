// Find-in-file for the code/markdown editor (proposal 0038, Part B).
//
// A floating, non-modal search widget over the CodeMirror surface: highlights
// ALL matches as you type (debounced, ≥3 chars), shows a live "n / N" count,
// moves between matches with Enter / Shift-Enter (and the ↑/↓ buttons), and
// carries case / whole-word / regex toggles with smart-case defaults. Built on
// `@codemirror/search` but with a fully custom panel (`createPanel`) so the look
// is ours — the package's match-highlight decorations (`.cm-searchMatch` /
// `.cm-searchMatch-selected`, themed in MarkdownEditor) do the on-document part.
//
// State lives in CodeMirror (the SearchQuery), not React. The EditorOverlay
// reaches it through the live `EditorView` (see `findIsOpen`/`closeFind` below)
// so its layered-Esc ladder can clear-then-close the widget without a second
// copy of the state. Nothing here fetches or mutates the document.

import { EditorView, keymap, type Panel, type ViewUpdate } from "@codemirror/view";
import { Prec, type EditorState, type Extension } from "@codemirror/state";
import {
  search,
  openSearchPanel,
  closeSearchPanel,
  searchPanelOpen,
  findNext,
  findPrevious,
  getSearchQuery,
  setSearchQuery,
  SearchQuery,
  SearchCursor,
  RegExpCursor,
} from "@codemirror/search";

// Live-highlight / tree-filter gate: below this many characters we don't paint
// every match (it would light up half the document on each keystroke). Enter
// still jumps to the first match below it. Mirrors the [0027]/[0016] threshold.
const MIN_HIGHLIGHT = 3;
// Counting cap so a 1-char-ish query on a huge file can't spin counting matches.
// Past it the count reads "N+"; navigation still works (it uses CM's own cursor).
const MATCH_CAP = 5000;

type Match = { from: number; to: number };

function isWordChar(ch: string): boolean {
  return ch !== "" && /[\p{L}\p{N}_]/u.test(ch);
}

// CM's whole-word rule: a match is kept only when it isn't flanked by more word
// characters on a side whose own edge is a word char. Approximated here so the
// "n / N" count agrees with what CM highlights when the W toggle is on.
function passesWholeWord(state: EditorState, m: Match): boolean {
  const doc = state.doc;
  const before = m.from > 0 ? doc.sliceString(m.from - 1, m.from) : "";
  const after = m.to < doc.length ? doc.sliceString(m.to, m.to + 1) : "";
  const first = doc.sliceString(m.from, m.from + 1);
  const last = doc.sliceString(m.to - 1, m.to);
  const startOk = !isWordChar(before) || !isWordChar(first);
  const endOk = !isWordChar(after) || !isWordChar(last);
  return startOk && endOk;
}

// Smart case: until the user explicitly clicks the case pill, case-sensitivity
// follows "does the query contain an uppercase letter". Exported (pure) for
// unit testing the rule in isolation.
export function smartCaseSensitive(query: string, userSetCase: boolean, manual: boolean): boolean {
  return userSetCase ? manual : /[A-Z]/.test(query);
}

// All matches of `q` in the document, capped. Returns null when the query is a
// regexp that fails to compile (→ the panel shows "Invalid pattern"). Exported
// for unit testing the count/whole-word/regex logic against an EditorState.
export function computeMatches(state: EditorState, q: SearchQuery): Match[] | null {
  const doc = state.doc;
  if (!q.search) return [];
  const out: Match[] = [];
  if (q.regexp) {
    let cur: RegExpCursor;
    try {
      cur = new RegExpCursor(doc, q.search, { ignoreCase: !q.caseSensitive });
    } catch {
      return null;
    }
    try {
      while (!cur.next().done) {
        out.push({ from: cur.value.from, to: cur.value.to });
        if (out.length >= MATCH_CAP) break;
      }
    } catch {
      return null;
    }
  } else {
    const norm = q.caseSensitive ? undefined : (s: string) => s.toLowerCase();
    const cur = new SearchCursor(doc, q.search, 0, doc.length, norm);
    while (!cur.next().done) {
      out.push({ from: cur.value.from, to: cur.value.to });
      if (out.length >= MATCH_CAP) break;
    }
  }
  return q.wholeWord ? out.filter((m) => passesWholeWord(state, m)) : out;
}

function svgIcon(path: string, size = 14): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("aria-hidden", "true");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", path);
  p.setAttribute("fill", "none");
  p.setAttribute("stroke", "currentColor");
  p.setAttribute("stroke-width", "2");
  p.setAttribute("stroke-linecap", "round");
  p.setAttribute("stroke-linejoin", "round");
  svg.appendChild(p);
  return svg;
}

// The floating widget. Plain DOM (CodeMirror panels aren't React) but tokened
// via the `cc-find*` classes in index.css.
function makeFindPanel(view: EditorView): Panel {
  const seed = getSearchQuery(view.state);
  const toggles = {
    caseSensitive: seed.caseSensitive,
    wholeWord: seed.wholeWord,
    regexp: seed.regexp,
  };
  // Smart case: until the user clicks the case pill, case-sensitivity follows
  // "does the query contain an uppercase letter". After a click it's manual.
  let userSetCase = false;
  let debounceId: number | null = null;

  const dom = document.createElement("div");
  dom.className = "cc-find";
  dom.setAttribute("role", "search");
  // Keep clicks inside the widget from bubbling into the editor (which would
  // move the caret / blur the field on the way).
  dom.addEventListener("mousedown", (e) => {
    if (e.target !== input) e.preventDefault();
  });

  const icon = svgIcon("M21 21l-4.3-4.3M11 18a7 7 0 100-14 7 7 0 000 14z");
  icon.classList.add("cc-find-glyph");

  const input = document.createElement("input");
  input.className = "cc-find-input";
  input.type = "text";
  input.setAttribute("main-field", "true");
  input.setAttribute("aria-label", "Find in file");
  input.placeholder = "Find in file";
  input.spellcheck = false;
  input.autocapitalize = "off";
  input.setAttribute("autocorrect", "off");

  const count = document.createElement("span");
  count.className = "cc-find-count";
  count.setAttribute("aria-live", "polite");

  const mkBtn = (label: string, glyphPath: string | null, text?: string) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "cc-find-btn";
    b.setAttribute("aria-label", label);
    b.title = label;
    if (glyphPath) b.appendChild(svgIcon(glyphPath, 16));
    else if (text) b.textContent = text;
    // [0009] guard: toggle on mousedown+preventDefault so a tap doesn't blur the
    // field / drop the soft keyboard mid-search.
    b.addEventListener("mousedown", (e) => e.preventDefault());
    return b;
  };

  const prevBtn = mkBtn("Previous match (Shift+Enter)", "M18 15l-6-6-6 6");
  const nextBtn = mkBtn("Next match (Enter)", "M6 9l6 6 6-6");

  const mkToggle = (label: string, glyph: string) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "cc-find-toggle";
    b.textContent = glyph;
    b.setAttribute("aria-label", label);
    b.setAttribute("aria-pressed", "false");
    b.title = label;
    b.addEventListener("mousedown", (e) => e.preventDefault());
    return b;
  };
  const caseBtn = mkToggle("Match case", "Aa");
  const wordBtn = mkToggle("Whole word", "W");
  const regexpBtn = mkToggle("Use regular expression", ".*");

  const closeBtn = mkBtn("Close (Esc)", "M6 6l12 12M18 6L6 18");
  closeBtn.classList.add("cc-find-close");

  const nav = document.createElement("div");
  nav.className = "cc-find-group";
  nav.append(prevBtn, nextBtn);
  const tgl = document.createElement("div");
  tgl.className = "cc-find-group cc-find-toggles";
  tgl.append(caseBtn, wordBtn, regexpBtn);
  const sep1 = document.createElement("span");
  sep1.className = "cc-find-sep";
  const sep2 = document.createElement("span");
  sep2.className = "cc-find-sep";

  dom.append(icon, input, count, nav, sep1, tgl, sep2, closeBtn);

  const effectiveCase = (text: string) =>
    smartCaseSensitive(text, userSetCase, toggles.caseSensitive);

  const buildQuery = (text: string) =>
    new SearchQuery({
      search: text,
      caseSensitive: effectiveCase(text),
      wholeWord: toggles.wholeWord,
      regexp: toggles.regexp,
      literal: !toggles.regexp,
    });

  const refreshToggleUI = () => {
    const cs = effectiveCase(input.value);
    caseBtn.classList.toggle("is-on", cs);
    caseBtn.setAttribute("aria-pressed", String(cs));
    // Smart-case (auto) vs manual gets a subtle hint via a data attribute the
    // CSS can dim.
    caseBtn.dataset.auto = userSetCase ? "0" : "1";
    wordBtn.classList.toggle("is-on", toggles.wholeWord);
    wordBtn.setAttribute("aria-pressed", String(toggles.wholeWord));
    regexpBtn.classList.toggle("is-on", toggles.regexp);
    regexpBtn.setAttribute("aria-pressed", String(toggles.regexp));
  };

  // Recompute the "n / N" readout (and the no-match / invalid states). `forced`
  // computes even below the highlight threshold (used after an Enter-to-jump).
  const updateCount = (forced = false) => {
    dom.classList.remove("cc-find--error", "cc-find--empty", "cc-find--hint");
    const text = input.value;
    if (!text) {
      count.textContent = "";
      return;
    }
    if (text.length < MIN_HIGHLIGHT && !forced) {
      count.textContent = "Keep typing…";
      dom.classList.add("cc-find--hint");
      return;
    }
    const q = buildQuery(text);
    if (q.regexp && !q.valid) {
      count.textContent = "Invalid pattern";
      dom.classList.add("cc-find--error");
      return;
    }
    const matches = computeMatches(view.state, q);
    if (matches === null) {
      count.textContent = "Invalid pattern";
      dom.classList.add("cc-find--error");
      return;
    }
    const n = matches.length;
    if (n === 0) {
      count.textContent = "No matches";
      dom.classList.add("cc-find--empty");
      return;
    }
    const sel = view.state.selection.main;
    const idx = matches.findIndex((m) => m.from === sel.from && m.to === sel.to);
    const capped = n >= MATCH_CAP ? "+" : "";
    count.textContent = idx >= 0 ? `${idx + 1} / ${n}${capped}` : `${n}${capped} match${n === 1 ? "" : "es"}`;
  };

  // Push the current query into CodeMirror so it highlights every match. Below
  // the threshold we clear it (no all-highlight) unless `force` (Enter-to-jump).
  const apply = (force = false) => {
    const text = input.value;
    if (text.length >= MIN_HIGHLIGHT || (force && text)) {
      const q = buildQuery(text);
      if (!q.regexp || q.valid) view.dispatch({ effects: setSearchQuery.of(q) });
    } else {
      view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "" })) });
    }
  };

  input.addEventListener("input", () => {
    refreshToggleUI();
    if (debounceId !== null) window.clearTimeout(debounceId);
    debounceId = window.setTimeout(() => {
      debounceId = null;
      apply();
      updateCount();
    }, 120);
  });

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!input.value) return;
      apply(true); // ensure a query exists even below the highlight threshold
      if (e.shiftKey) findPrevious(view);
      else findNext(view);
      updateCount(true);
    }
    // Escape is owned by the overlay's layered-Esc ladder (a window
    // capture-phase handler that runs first); we don't handle it here.
  });

  prevBtn.addEventListener("click", () => {
    findPrevious(view);
    updateCount(true);
    input.focus();
  });
  nextBtn.addEventListener("click", () => {
    findNext(view);
    updateCount(true);
    input.focus();
  });

  const onToggle = (mut: () => void) => () => {
    mut();
    refreshToggleUI();
    apply(true);
    updateCount(true);
    input.focus();
  };
  caseBtn.addEventListener(
    "click",
    onToggle(() => {
      userSetCase = true;
      toggles.caseSensitive = !effectiveCase(input.value);
    })
  );
  wordBtn.addEventListener("click", onToggle(() => (toggles.wholeWord = !toggles.wholeWord)));
  regexpBtn.addEventListener("click", onToggle(() => (toggles.regexp = !toggles.regexp)));
  closeBtn.addEventListener("click", () => closeFind(view));

  return {
    dom,
    top: true,
    mount() {
      const cur = getSearchQuery(view.state);
      if (cur.search) {
        input.value = cur.search;
        toggles.caseSensitive = cur.caseSensitive;
        toggles.wholeWord = cur.wholeWord;
        toggles.regexp = cur.regexp;
        userSetCase = true;
      } else {
        const sel = view.state.selection.main;
        if (!sel.empty) {
          const text = view.state.sliceDoc(sel.from, sel.to);
          if (text && !text.includes("\n") && text.length <= 200) {
            input.value = text;
            apply();
          }
        }
      }
      refreshToggleUI();
      updateCount(true);
      requestAnimationFrame(() => {
        input.focus();
        input.select();
      });
    },
    update(u: ViewUpdate) {
      // Sync the field when the query changes from OUTSIDE the panel (the
      // overlay's Esc-to-clear) so the input never drifts from the state.
      let externalClear = false;
      for (const tr of u.transactions) {
        for (const ef of tr.effects) {
          if (ef.is(setSearchQuery)) {
            const q = ef.value as SearchQuery;
            if (q.search !== input.value) {
              input.value = q.search;
              externalClear = true;
            }
          }
        }
      }
      if (u.docChanged || u.selectionSet) updateCount(true);
      else if (externalClear) updateCount();
    },
    destroy() {
      if (debounceId !== null) window.clearTimeout(debounceId);
    },
  };
}

// The extension bundle to push onto the editor: the search support (custom
// panel) plus a high-precedence Mod-f to open it (beats CM defaults; the overlay
// owns Esc/save at capture phase).
export function findInFileExtensions(): Extension {
  return [
    search({ top: true, literal: true, createPanel: makeFindPanel }),
    Prec.high(keymap.of([{ key: "Mod-f", run: openSearchPanel }])),
  ];
}

// Imperative handles the overlay drives (toolbar 🔎 button + Esc ladder).
export function openFind(view: EditorView): void {
  openSearchPanel(view);
}
export function findIsOpen(state: EditorState): boolean {
  return searchPanelOpen(state);
}
export function findHasQuery(state: EditorState): boolean {
  return getSearchQuery(state).search.length > 0;
}
// Clear the query (and its highlights) but keep the panel open + focused — the
// first rung of the Esc ladder.
export function clearFindQuery(view: EditorView): void {
  view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "" })) });
}
// Clear then close — the second rung, and the ✕ button.
export function closeFind(view: EditorView): void {
  view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "" })) });
  closeSearchPanel(view);
  view.focus();
}
