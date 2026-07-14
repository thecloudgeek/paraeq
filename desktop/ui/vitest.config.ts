import { defineConfig } from "vitest/config";

// Pure-math unit tests (FreqPlotRenderer) — no DOM/canvas required.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
