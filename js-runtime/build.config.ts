// Builds the two runtime bundles consumed by the Rust crate via
// `include_str!`. Agent E owns the source layout under src/; this entry
// point stays tiny so the build surface is stable.

import { build } from "esbuild";
import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const outDir = resolve(
  __dirname,
  "..",
  "crates",
  "tauri-plugin-extensions",
  "embedded-js",
);

async function main(): Promise<void> {
  await mkdir(outDir, { recursive: true });

  const common = {
    bundle: true,
    format: "iife" as const,
    platform: "browser" as const,
    target: ["chrome120"],
    logLevel: "info" as const,
    sourcemap: false,
    minify: false,
  };

  await Promise.all([
    build({
      ...common,
      entryPoints: [resolve(__dirname, "src", "content", "bootstrap.ts")],
      outfile: resolve(outDir, "content-bootstrap.js"),
      globalName: "__TauriExtContent",
    }),
    build({
      ...common,
      entryPoints: [resolve(__dirname, "src", "background", "bootstrap.ts")],
      outfile: resolve(outDir, "background-bootstrap.js"),
      globalName: "__TauriExtBackground",
    }),
  ]);

  console.log(`built → ${outDir}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
