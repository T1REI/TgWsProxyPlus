import { defineConfig } from "astro/config";
import preact from "@astrojs/preact";

export default defineConfig({
  output: "static",
  integrations: [preact()],
  vite: { build: { sourcemap: false, target: "es2022" } },
});
