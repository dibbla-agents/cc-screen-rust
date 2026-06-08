import { ChatBubbleIcon } from "../icons";

// Toggle for in-app session-ready toasts (proposal 0017) — the foreground
// complement to the OS Web Push bell (NotificationsButton) it sits beside.
// Purely a controlled switch: App owns the persisted on/off flag and, on
// enable, fires a one-off **test toast** so the user immediately sees what a
// real ready-notification will look like (mirroring the bell's test buzz). The
// setting persists in localStorage; toasts default on.
export default function ToastsButton({
  on,
  onToggle,
  className,
}: {
  on: boolean;
  onToggle: () => void;
  className?: string;
}) {
  const title = on
    ? "In-app ready-toasts on — tap to turn off"
    : "Show an in-app toast when a session finishes (sends a test toast)";
  return (
    <button onClick={onToggle} aria-label="Toggle in-app ready-toasts" title={title} className={className}>
      <ChatBubbleIcon className={`h-4 w-4 ${on ? "text-accent" : ""}`} filled={on} off={!on} />
    </button>
  );
}
