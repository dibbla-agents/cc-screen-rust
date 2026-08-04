import { Brand } from "./Nav";
import { APP, DOCS, GITHUB } from "../urls";

export function Footer() {
  return (
    <footer className="mx-auto flex max-w-[820px] flex-col gap-3 border-t border-line-soft px-6 pb-14 pt-9">
      <Brand />
      <nav className="flex flex-wrap gap-x-5 gap-y-2 font-mono text-[0.82rem] text-dim">
        <a className="hover:text-green-soft" href={APP}>
          Sign up
        </a>
        <a className="hover:text-green-soft" href={DOCS}>
          Docs
        </a>
        <a className="hover:text-green-soft" href="#self-host">
          Self-host
        </a>
        <a className="hover:text-green-soft" href={GITHUB}>
          GitHub
        </a>
        <a className="hover:text-green-soft" href={`${GITHUB}/releases`}>
          Releases
        </a>
        <a className="hover:text-green-soft" href={`${GITHUB}#readme`}>
          README
        </a>
      </nav>
      <p className="font-mono text-[0.76rem] text-faint">
        Built in Rust — for people who keep their agents on.
      </p>
    </footer>
  );
}
