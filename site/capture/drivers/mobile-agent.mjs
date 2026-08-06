// The Web app claude session on a phone. Attach and let claude repaint at
// the phone's PTY size before framing.
export default async (page) => {
  await page.locator("text=Web app").first().click();
  await page.waitForTimeout(4000);
};
