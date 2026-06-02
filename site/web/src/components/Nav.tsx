const GITHUB = "https://github.com/dibbla-agents/cc-screen-rust";

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

export function Nav() {
  return (
    <header className="sticky top-0 z-10 border-b border-line-soft bg-[rgba(6,14,9,0.72)] backdrop-blur-[10px]">
      <div className="mx-auto flex h-15 max-w-[820px] items-center justify-between px-4 sm:px-6">
        <Brand />
        <nav className="flex gap-4 font-mono text-[0.78rem] text-dim sm:gap-6 sm:text-[0.82rem]">
          <a className="hover:text-green-soft" href="#features">
            Features
          </a>
          <a className="hover:text-green-soft" href="#apps">
            Apps
          </a>
          <a className="hover:text-green-soft" href="#start">
            Start
          </a>
          <a className="hover:text-green-soft" href={GITHUB}>
            GitHub&nbsp;↗
          </a>
        </nav>
      </div>
    </header>
  );
}

export { GITHUB, Brand };
