import type { ReactNode } from "react";
import { APP, DOCS, GITHUB } from "../urls";

function Brand() {
  return (
    <a
      href="#top"
      className="inline-flex items-center gap-[0.55ch] whitespace-nowrap font-mono font-bold tracking-[-0.02em]"
    >
      <span className="text-green">&gt;_</span>cc-screen
    </a>
  );
}

/* One nav link with a ≥44px tap target (min-h-11 inside the 60px bar). `wide`
   hides the link on <sm — on phones only Docs, GitHub and the Sign-up button
   survive; the button is the funnel and never collapses. */
function NavLink({
  href,
  wide = false,
  children,
}: {
  href: string;
  wide?: boolean;
  children: ReactNode;
}) {
  return (
    <a
      className={`${wide ? "hidden sm:inline-flex" : "inline-flex"} min-h-11 items-center px-1 hover:text-green-soft`}
      href={href}
    >
      {children}
    </a>
  );
}

export function Nav() {
  return (
    <header className="sticky top-0 z-10 border-b border-line-soft bg-[rgba(6,14,9,0.72)] backdrop-blur-[10px]">
      <div className="mx-auto flex h-15 max-w-[820px] items-center justify-between gap-3 px-4 sm:px-6">
        <Brand />
        <nav className="flex items-center gap-2.5 font-mono text-[0.78rem] text-dim sm:gap-4 sm:text-[0.82rem]">
          <NavLink wide href="#how">
            How it works
          </NavLink>
          <NavLink wide href="#features">
            Features
          </NavLink>
          <NavLink wide href="#pricing">
            Pricing
          </NavLink>
          <NavLink href={DOCS}>Docs</NavLink>
          <NavLink href={GITHUB}>GitHub&nbsp;↗</NavLink>
          <a
            href={APP}
            className="inline-flex min-h-11 items-center whitespace-nowrap rounded-lg bg-green px-3 py-1.5 font-bold text-[#06120a] transition-colors hover:bg-green-soft"
          >
            Sign up
          </a>
        </nav>
      </div>
    </header>
  );
}

export { Brand };
