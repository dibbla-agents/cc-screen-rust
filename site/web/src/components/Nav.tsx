import { useEffect, useState, type ReactNode } from "react";
import { APP, DOCS, GITHUB } from "../urls";

/* The app's wordmark (MultiTenant.tsx:101-112): `cc·screen`, mono semibold,
   tight tracking, the middle dot in accent cyan. One mark on every surface. */
function Brand() {
  return (
    <a
      href="#top"
      className="inline-flex items-center whitespace-nowrap font-mono text-[0.95rem] font-semibold tracking-tight text-ink"
    >
      cc<span className="text-accent">·</span>screen
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
      className={`${wide ? "hidden sm:inline-flex" : "inline-flex"} min-h-11 items-center px-1 transition-colors duration-150 hover:text-accent`}
      href={href}
    >
      {children}
    </a>
  );
}

export function Nav() {
  // The bar starts borderless over the hero glow and grows a hairline only once
  // the page has scrolled past it (B7) — no fixed frame boxing the hero in.
  const [scrolled, setScrolled] = useState(false);
  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 8);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  return (
    <header
      className={`sticky top-0 z-10 border-b bg-[rgba(11,17,26,0.75)] backdrop-blur-[10px] transition-colors duration-200 ${
        scrolled ? "border-line-soft" : "border-transparent"
      }`}
    >
      <div className="mx-auto flex h-15 max-w-[1080px] items-center justify-between gap-3 px-4 sm:px-6">
        <Brand />
        <nav className="flex items-center gap-2.5 font-mono text-[0.78rem] text-dim sm:gap-4 sm:text-[0.82rem]">
          <NavLink wide href="#how">
            How it works
          </NavLink>
          <NavLink wide href="#tour">
            Tour
          </NavLink>
          <NavLink wide href="#pricing">
            Pricing
          </NavLink>
          <NavLink href={DOCS}>Docs</NavLink>
          <NavLink href={GITHUB}>GitHub&nbsp;↗</NavLink>
          <a
            href={APP}
            className="inline-flex min-h-11 items-center whitespace-nowrap rounded-lg bg-accent px-3 py-1.5 font-semibold text-[#0b111a] transition hover:brightness-110"
          >
            Sign up
          </a>
        </nav>
      </div>
    </header>
  );
}

export { Brand };
