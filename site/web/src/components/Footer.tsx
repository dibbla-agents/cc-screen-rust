import { Brand } from "./Nav";
import { APP, DOCS, GITHUB } from "../urls";

export function Footer() {
  return (
    <footer className="mx-auto flex max-w-[1080px] flex-col gap-3 px-6 pb-14 pt-12">
      <div className="flex flex-col gap-1.5">
        <Brand />
        {/* the app's tagline idiom — spaced uppercase (MultiTenant.tsx:109-111) */}
        <span className="font-mono text-[11px] uppercase tracking-[0.25em] text-faint">
          agents, anywhere
        </span>
      </div>
      <nav className="mt-2 flex flex-wrap gap-x-5 gap-y-2 font-mono text-[0.82rem] text-dim">
        <a className="transition-colors hover:text-accent" href={APP}>
          Sign up
        </a>
        <a className="transition-colors hover:text-accent" href={DOCS}>
          Docs
        </a>
        <a className="transition-colors hover:text-accent" href="#self-host">
          Self-host
        </a>
        <a className="transition-colors hover:text-accent" href={GITHUB}>
          GitHub
        </a>
        <a
          className="transition-colors hover:text-accent"
          href={`${GITHUB}/releases`}
        >
          Releases
        </a>
        <a className="transition-colors hover:text-accent" href={`${GITHUB}#readme`}>
          README
        </a>
      </nav>
      <p className="font-mono text-[0.76rem] text-faint">
        Built in Rust — for people who keep their agents on.
      </p>
    </footer>
  );
}
