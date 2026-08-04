import { Nav } from "./components/Nav";
import { Hero } from "./components/Hero";
import { HowItWorks } from "./components/HowItWorks";
import { Features } from "./components/Features";
import { Apps } from "./components/Apps";
import { Demo } from "./components/Demo";
import { Pricing } from "./components/Pricing";
import { Start } from "./components/Start";
import { Footer } from "./components/Footer";

export function App() {
  return (
    <>
      <Nav />
      <main id="top">
        <Hero />
        <HowItWorks />
        <Features />
        <Apps />
        <Demo />
        <Pricing />
        <Start />
      </main>
      <Footer />
    </>
  );
}
