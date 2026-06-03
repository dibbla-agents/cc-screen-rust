// The gate shown when the server has auth on and we're not yet authenticated
// (App.tsx renders this instead of the app). One field accepts either the
// password or the API token; a correct secret makes the server set a 2-week
// session cookie, and onSuccess re-checks auth and reveals the app.

import { useState } from "react";
import { login } from "../api";

export default function LoginScreen({ onSuccess }: { onSuccess: () => void }) {
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!secret || busy) return;
    setBusy(true);
    setError(false);
    try {
      if (await login(secret)) {
        onSuccess();
      } else {
        setError(true);
        setSecret("");
      }
    } catch {
      setError(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 flex items-center justify-center bg-bar px-6 text-slate-100">
      <form
        onSubmit={submit}
        className="w-full max-w-xs rounded-2xl border border-edge bg-panel p-6 shadow-2xl"
      >
        <div className="mb-5 flex flex-col items-center gap-2 text-center">
          <div className="flex h-12 w-12 items-center justify-center rounded-full border border-edge bg-bar text-2xl">
            🔒
          </div>
          <h1 className="font-mono text-base font-semibold text-slate-100">cc-screen</h1>
          <p className="text-xs text-slate-500">Enter your password or API token</p>
        </div>

        <input
          autoFocus
          type="password"
          value={secret}
          onChange={(e) => {
            setSecret(e.target.value);
            setError(false);
          }}
          placeholder="Password or token"
          autoComplete="current-password"
          className="w-full rounded-md border border-edge bg-bar px-3 py-2.5 text-sm text-slate-100 outline-none focus:border-accent"
        />

        {error && (
          <div className="mt-2 text-center text-xs text-red-400">
            Incorrect — try again.
          </div>
        )}

        <button
          type="submit"
          disabled={!secret || busy}
          className="mt-4 w-full rounded-md bg-accent px-3 py-2.5 text-sm font-semibold text-bar transition disabled:opacity-40"
        >
          {busy ? "Checking…" : "Unlock"}
        </button>
      </form>
    </div>
  );
}
