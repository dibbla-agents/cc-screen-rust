// The Web Push bell lives in the session switcher's header. The enabled
// (filled) state is NOT automatable: Chromium's push service checks real
// content settings, so PushManager.subscribe throws "Registration failed -
// permission denied" under Playwright's CDP permission override even with
// Notification.permission granted. The honest frame is the bell in its
// resting state, hovered (real hover chrome) so the eye lands on it; the
// short viewport crops the switcher to its header + first rows.
export default async (page) => {
  await page.locator("text=Web app").first().waitFor();
  await page.locator('[aria-label="Toggle notifications"]').hover();
  await page.waitForTimeout(400);
};
