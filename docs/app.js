// Copy-to-clipboard for command blocks. That's the only script on the page.
for (const btn of document.querySelectorAll(".copy")) {
  btn.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(btn.dataset.clip || "");
      const prev = btn.textContent;
      btn.textContent = "copied ✓";
      btn.classList.add("done");
      setTimeout(() => { btn.textContent = prev; btn.classList.remove("done"); }, 1400);
    } catch {
      btn.textContent = "copy failed";
      setTimeout(() => { btn.textContent = "copy"; }, 1400);
    }
  });
}
