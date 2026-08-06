// docs-cast.js — the docs cast player (proposal 0061 D3). The landing page's
// Demo.tsx player ported to vanilla JS: replay an asciinema v2 cast's "o"
// events into a <pre class="cast" data-cast="…"> on a requestAnimationFrame
// clock. No dependencies, no ANSI handling — the committed casts are plain
// text. The pre's authored content is the no-JS poster (a final-frame excerpt)
// and is restored if the fetch fails. Never autoplays; under
// prefers-reduced-motion a click shows the finished transcript instantly
// (mirrors Demo.tsx). Loaded `defer` by the shell only on pages that declare
// `casts: true` in front matter; styles live in docs.css (.cast-*).

/* Parse an asciinema v2 file: a JSON header line, then [time, "o", data]
   JSON-lines. Non-"o" events are ignored. */
function parseCast(raw) {
  const events = [];
  for (const line of raw.split("\n")) {
    const s = line.trim();
    if (!s.startsWith("[")) continue; // header or blank
    try {
      const ev = JSON.parse(s);
      if (ev[1] === "o") events.push({ t: ev[0], data: ev[2] });
    } catch {
      /* skip malformed lines */
    }
  }
  return events;
}

function setup(pre) {
  const src = pre.dataset.cast;
  if (!src) return;
  const poster = pre.textContent;

  // wrap so the play/replay overlays can absolutely position over the pre
  const box = document.createElement("div");
  box.className = "cast-box";
  pre.parentNode.insertBefore(box, pre);
  box.appendChild(pre);

  function finished() {
    // small corner control so the finished transcript stays readable
    const replay = document.createElement("button");
    replay.type = "button";
    replay.className = "cast-replay";
    replay.textContent = "↻ replay";
    replay.addEventListener("click", () => {
      replay.remove();
      play();
    });
    box.appendChild(replay);
  }

  async function play() {
    let raw;
    try {
      const res = await fetch(src);
      if (!res.ok) throw new Error(String(res.status));
      raw = await res.text();
    } catch {
      pre.textContent = poster; // fetch failed: the fallback frame stands
      return;
    }
    const events = parseCast(raw);
    if (events.length === 0) {
      pre.textContent = poster;
      return;
    }
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      // user asked for no motion: show the finished transcript instantly
      pre.textContent = events.map((e) => e.data).join("");
      finished();
      return;
    }
    pre.textContent = "";
    const start = performance.now();
    let next = 0;
    const tick = (now) => {
      const elapsed = (now - start) / 1000;
      let chunk = "";
      while (next < events.length && events[next].t <= elapsed) {
        chunk += events[next].data;
        next++;
      }
      if (chunk) {
        pre.textContent += chunk;
        pre.scrollTop = pre.scrollHeight; // keep the newest output in view
      }
      if (next < events.length) requestAnimationFrame(tick);
      else finished();
    };
    requestAnimationFrame(tick);
  }

  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "cast-play";
  btn.innerHTML = '<span class="glyph">▶</span> watch the install';
  btn.addEventListener(
    "click",
    () => {
      btn.remove();
      play();
    },
    { once: true },
  );
  box.appendChild(btn);
}

document.addEventListener("DOMContentLoaded", () => {
  document.querySelectorAll("pre.cast[data-cast]").forEach(setup);
});
