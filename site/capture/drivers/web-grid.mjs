// Three panes + the layout palette open (B4). Click-driven throughout: the
// Ctrl+B prefix is swallowed while an empty pane's autofocused switcher
// search box has focus, so chords are unreliable here.
export default async (page) => {
  // Pane 1: mount Web app via the full-screen empty-pane switcher.
  await page.locator("text=Web app").first().click();
  await page.waitForTimeout(2500);

  const openPalette = async () => {
    await page.mouse.move(720, 2); // summon the collapsed header
    await page.waitForTimeout(700);
    await page.locator("[data-layout-trigger]").click();
    await page.waitForTimeout(500);
  };

  // Apply the 3-pane "Left-tall L" layout (click applies and closes).
  await openPalette();
  await page.locator('[aria-label="Left-tall L"]').click();
  await page.waitForTimeout(1200);

  // Panes 2 and 3 are empty inline switchers (DOM order). Mount deploy in
  // pane 2 first — "deploy" appears in every switcher's list, and the first
  // match is pane 2's — then Docs site in the one remaining switcher.
  // exact: the substring form ("text=deploy") can first-match the word
  // "deployment" inside a live claude transcript (terminal rows are DOM).
  await page.getByText("deploy", { exact: true }).first().click();
  await page.waitForTimeout(2000);
  await page.locator("text=Docs site").first().click();
  await page.waitForTimeout(3500);

  // Leave the palette open for the frame (hover keeps it from applying).
  await openPalette();
  await page.waitForTimeout(800);
};
