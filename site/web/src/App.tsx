import { Nav } from "./components/Nav";
import { Hero } from "./components/Hero";
import { Features } from "./components/Features";
import { Apps } from "./components/Apps";
import { Start } from "./components/Start";
import { Footer } from "./components/Footer";

export function App() {
  return (
    <>
      <Nav />
      <main id="top">
        <Hero />
        <Features />
        <Apps />
        <Start />
      </main>
      <Footer />
    </>
  );
}
