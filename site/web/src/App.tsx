import { Nav } from "./components/Nav";
import { Hero } from "./components/Hero";
import { HowItWorks } from "./components/HowItWorks";
import { Tour } from "./components/Tour";
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
        <Tour />
        <Demo />
        <Pricing />
        <Start />
      </main>
      <Footer />
    </>
  );
}
