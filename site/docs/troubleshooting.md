---
title: Troubleshooting
nav: Troubleshooting
description: Symptom-shaped fixes — activation codes, offline machines, missing assistants, plan limits, invite email that never arrived, an editor stuck on "path outside home", and proxy 403s.
---

# Troubleshooting

Symptom first, fix second. Self-hosting problems (uplink tokens, TLS,
binds) have their own section on the [Self-hosting](../self-hosting/) page.

## "The activation code expired"

Codes live **10 minutes**. There's nothing to redo: the installer notices the
expiry and prints a fresh code by itself —

```
  Code expired before approval — requesting a new one…
```

— so just enter the new one at
[app.ccscreen.dev/activate](https://app.ccscreen.dev/activate). If you closed
the installer, re-run the one-liner; it's idempotent.

## "Approve machine" says login required

Approving binds the machine to *your* account, so the browser on `/activate`
must be signed in. Sign in first, then enter the code — on the same site,
any device.

## A session fails to start / an assistant isn't offered

The machine doesn't have that coding CLI installed. cc-screen tells you
rather than launching a doomed session: the picker greys the tool out, and
the machine's dashboard row shows **⚠ N missing · Install** — click it to
install the missing CLIs remotely (for that machine's user, no sudo). On the
box itself, the equivalent is:

```sh
cc-screen-rust doctor                  # what's installed, and where
cc-screen-rust doctor --install --yes  # install everything missing
```

On Windows, remember Codex and Gemini need Node.js first — see
[Install on Windows](../install-windows/).

## A machine shows offline

Its agent's connection to the hub dropped — the box is asleep, offline, or
rebooting. Nothing is lost: the agent owns its sessions locally, keeps them
running, and reconnects with backoff the moment it can. Sessions re-attach by
themselves. If it stays offline, check that the machine is actually up and
that the background service is running (`systemctl --user status
cc-screen-rust` on Linux).

## "Machine limit reached for your plan"

You hit your plan's cap. The app shows which cap you hit and your current plan,
and offers the upgrade right there — checkout to Pro happens in-app. Or unlink a
machine (or stop a session) you no longer need. **Nothing is deleted** when you
hit a limit: existing machines stay enrolled, running sessions keep running, and
your files are never touched — the cap only blocks *new* machines and sessions
until you're back under it.

## "This team is out of seats"

Accepting the invite would take the team past its paid seat count. The
team's **owner or an admin** fixes it — add seats from Billing (the Stripe
portal), or remove a member to free one. **Nothing was lost**: the invite
stays valid and the accept simply works once a seat is free.

## "Team machine pool full" / "Team session limit reached"

Team limits are **pooled**: 10 machines and 50 concurrent sessions per seat,
counted across the whole team — so you can hit the cap even if *you* have
only one machine, because a teammate is using the headroom. Same rule as
every limit: nothing is deleted; existing machines stay enrolled and running
sessions keep running. Free the pool (unlink a machine, stop a session,
anyone's) or have the owner add seats. Re-enrolling a machine the team
already has never counts against the pool.

## A teammate's machine isn't visible

Three usual causes, in order:

1. **They hid it.** Each machine has an owner-only "Visible to team" toggle —
   ask them to check the team window.
2. **Membership isn't active yet.** An invited-but-not-accepted member sees
   (and shows) nothing — the invite must be accepted, and the team needs a
   free seat for that.
3. **A freshly enrolled machine hasn't propagated.** Normally visibility is
   immediate; in the rare miss, a nightly reconcile heals it. If it's still
   missing the next day, that's a bug — please report it.

## The invite email never arrived

Four usual causes, in order:

1. **It's in the spam folder.** A first message from a sender the recipient
   has never heard from often lands there. Have them search for
   `invites@ccscreen.dev` before anything else.
2. **The address has a typo.** The invitation is stored under exactly what
   was typed, so `@gmial.com` produces a perfectly valid pending invite that
   can never arrive. Cancel it and invite again.
3. **The hub doesn't send mail.** A self-hosted hub mails nothing unless its
   operator configured a relay (`CCHUB_SMTP_URL` — see
   [Self-hosting](../self-hosting/#environment-reference)). Nothing else
   about invites changes; the link is the channel.
4. **The send failed.** Pending invite rows carry a delivery badge —
   *sending*, *sent*, *failed*, or *bad address*. A **failed** row gets a
   **Resend** button. **bad address** means the relay refused that address
   permanently (no such mailbox, or no such domain); resending can't fix it,
   so cancel the invite and re-issue it to the right address.

And the answer that always works, in all four cases: **Copy link** next to
the invite and send it yourself. It's the same invitation the email carries.
One caveat on the badge — *sent* means the relay accepted the message, not
that it reached a mailbox; cc-screen doesn't yet hear back about bounces.

## Assistants are outdated

Use the **Update** button on the machine's dashboard row — it updates the
CLIs first (a failed update leaves the machine untouched), then gracefully
restarts the sessions that use them, and your panes re-attach under the same
names. On the box: `cc-screen-rust doctor --update` (updates the CLIs only,
restarts nothing).

## Codex says "clipboard unavailable" / an X11 error on image paste

Older cc-screen agents delivered every pasted image the same way, and Codex
tried to read it from the machine's X11/Wayland clipboard — which a headless
box doesn't have, so the paste failed with a clipboard/X11 connection error.
Current agents deliver images to Codex as a staged local file instead, so
**update the agent on that machine** (the machine dashboard row → Update, or
re-run the install one-liner) and the paste works with no display server.
You do **not** need to install Xvfb or any X11 packages.

## Notifications don't arrive

- On iOS, Web Push only works for a PWA **installed to the home screen** —
  the notifications button hides until then.
- Re-check the permission: the bell button shows whether the subscription is
  active, and has a "send a test" action.

![The notification bell in the switcher header](../img/mobile-notifications.png "The bell lives in the switcher header — it shows whether push is active and can send a test notification.")

## Swiping does nothing on my phone

Same cause as the entry below, in the web app. Claude Code's **fullscreen
renderer** draws on the terminal's alternate screen — which keeps no scrollback
at all — and takes the mouse, so a swipe had nothing to move. **cc-screen 0.5.6
and newer** hand the swipe to the application instead, so a drag scrolls
*Claude's own* transcript, and a flick coasts. Reload the tab (twice, if the
PWA served you the old bundle) after your hub has been updated.

The trap is that this is **rolled out per install**, not per version: the same
Claude Code build can use the fullscreen renderer on one machine and the classic
one on another, and it can flip on a machine at any time — the decision is
cached in `~/.claude.json`, and Claude persists `"tui"` into
`~/.claude/settings.json` once the renderer has been active. So "it works on my
other machine" says nothing. If you're stuck on an older cc-screen, `/tui
default` inside Claude switches that install back to the classic renderer.

## Scrolling does nothing in a Claude session

In a `ccs` pane, that's Claude Code's **fullscreen renderer** (Claude Code
2.1.89 and newer, and the default for fresh installs): it draws on the
terminal's alternate screen and owns the mouse itself, so the pane has no
local scrollback there and older `ccs` builds swallowed the wheel instead of
passing it on. **`ccs` 0.5.4 and newer forward the wheel to the app**, so it
scrolls Claude's own view — `ccs update` is the fix. The same applies to any
full-screen app (`htop`, `lazygit`, `less`, `vim`); see
[the ccs terminal client](../tui/#scrolling) for the full rules.

Stuck on an older client? Switch Claude back to its classic renderer, which
prints into the normal screen and scrolls with the pane — inside Claude:

```
/tui default
```

(or `"tui": "default"` in `~/.claude/settings.json`). Launching it with
`CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` has the same effect.

## Ctrl+F doesn't find anything in a terminal

It can't: terminals are drawn on the GPU, so the browser's Find sees no text
there (it still works for the rest of the page). Use **`Ctrl+B /`** — the
find bar over the focused pane — or the magnifier button in the header on a
phone. Enter / Shift+Enter cycle matches, Esc closes.

## A `Ctrl+B` shortcut does nothing

Four things silently switch the prefix off, in rough order of likelihood:

- **The caret is in a text field.** Compose box, a search box, a rename field —
  while one of those has focus, `Ctrl+B` belongs to it, by design. Click the
  terminal (or press Esc) and try again.
- **The window is too narrow, or it's a touch screen.** The chords are
  desktop-only: they need a mouse-class pointer and a window at least 900 px
  wide. Narrow the window past that and they quietly stop — there's one pane
  and nothing to navigate between.
- **The layout palette is open.** While it's up it owns the keyboard; pick a
  layout or press Esc first.
- **You waited too long.** The prefix expires after about 600 ms — press
  `Ctrl+B` and the second key in one motion. (Held on its own, `Ctrl+B` opens
  the session list, which is what a long pause looks like.)

The full list of chords is in [the web app guide](../web-app/#keyboard-shortcuts).

## I copied inside Claude and my clipboard is empty

Which machine's clipboard? That's the whole question.

Every copy Claude Code performs — `/copy`, copy-on-select, `Ctrl+C` on a
selection inside its own UI, `/export → Clipboard` — is emitted to the terminal
for the terminal to act on. **cc-screen 0.5.6 and newer act on it**: the text
lands on the clipboard of the device you're actually holding. Older clients
parsed the sequence and threw it away, so the assistant reported success and
nothing happened.

On a **macOS agent** it was worse than empty: Claude Code also shells out to
`pbcopy`, that write succeeds, and the text ends up on the *agent machine's*
pasteboard — a machine you're not sitting at, where anything running there can
read it. The agent now shims `pbcopy`/`wl-copy` for its own sessions and sends
the copy to you instead. Update the agent (`cc-screen-rust update`) and start a
new session; existing sessions keep the PATH they were launched with.

If the copy still doesn't arrive:

- **Are you driving that session?** A silent write only happens for the pane
  you're focused on and have typed into recently. Otherwise you get a **Copy**
  button — look for it above the bottom bar.
- **Multi-line or unusual text** always takes the button, deliberately.
- **In `ccs`**, the copy is handed to *your* terminal emulator, which has to
  support OSC 52 and have it enabled. iTerm2 gates it behind
  *Settings → General → Selection → "Applications in terminal may access
  clipboard"*. kitty, WezTerm and Windows Terminal allow it by default.
- **Under `tmux` or `screen`** the sequence is wrapped in a passthrough the web
  client doesn't unwrap, so that case still doesn't arrive. Known gap.
- **Nothing arrives from before you attached.** The replay a fresh attach sends
  is a picture of the screen, not the byte stream, so a copy made while no
  client was attached cannot be recovered — and a replay is deliberately not
  allowed to write your clipboard minutes after the fact.

## The terminal text looks slightly different

cc-screen renders terminals with WebGL where the browser supports it, and
falls back to plain DOM rendering where it doesn't (older iOS Safari, some
VMs, a machine with no GPU access). Antialiasing differs a little between the
two; everything else — selection, links, scrollback, search — behaves the
same, and no action is needed. (On a **phone**, read "selection" narrowly: touch
selection of terminal text isn't offered under either renderer. Copying out of a
session on a phone is the assistant's own copy landing on your clipboard, or the
copy button in the markdown viewer.) To see which one you're on, open the browser
console and type:

```js
window.__ccRenderer   // "webgl" or "dom"
```

A tab that starts on WebGL can move to `"dom"` on its own if the browser
drops the GPU context (iOS does this when a tab is backgrounded). That's the
fallback working as designed, not a fault.

## "path outside home" when opening the editor

Nothing left your home directory. That message is what an **older machine**
answers for a file that has simply *moved* — you renamed the folder your session
is working in, or deleted the file — and it keeps answering it every time you
open the editor, because the editor remembers the file you had open.

**Fix:** update the agent on that machine (`cc-screen-rust update`, then start a
fresh session, or use the machine's dashboard row). A current machine says "not found"
for a path that's gone, and a current web app repairs it for you: it reopens the
same file at its new location after a folder rename, and drops to the folder with
a grey notice when the file is really gone. In the meantime, clicking any file in
the tree clears the banner for that session.

## The page looks stale after an update

The web app is a PWA; after a deploy the service worker can serve the old
bundle for one more load. Close and reopen the app (or reload twice).

## Browser gets 403 behind my own proxy (self-host)

Your hub rejects unknown browser origins as an anti-DNS-rebinding measure.
Add your domain to `CCWEB_ALLOWED_ORIGINS` — see
[Self-hosting](../self-hosting/#off-tailnet-access-tls-required).

## Still stuck?

Check the [FAQ](../faq/), or open an issue at
[github.com/dibbla-agents/cc-screen-rust](https://github.com/dibbla-agents/cc-screen-rust).
