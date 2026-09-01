import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { spawn } from "node:child_process";

const packageRoot = resolve("node_modules/ag-psd");
const declaration = resolve(packageRoot, "src/canvas.d.ts");

// The published package provides canvas at runtime, but the oracle supplies
// image data directly and never calls the canvas adapter. Keep a type-only
// declaration so the pinned Git source can be compiled without native canvas.
await mkdir(dirname(declaration), { recursive: true });
await writeFile(
  declaration,
  'declare module "canvas" { export const createCanvas: any; }\n',
);

const compiler = resolve("node_modules/typescript/bin/tsc");
const child = spawn(
  process.execPath,
  [compiler, "--project", "node_modules/ag-psd/tsconfig.json"],
  { stdio: "inherit", shell: false },
);

const exitCode = await new Promise((resolveCode, reject) => {
  child.once("error", reject);
  child.once("exit", (code, signal) => {
    if (signal) {
      reject(new Error(`TypeScript compiler terminated by ${signal}`));
    } else {
      resolveCode(code ?? 1);
    }
  });
});
if (exitCode !== 0) {
  process.exitCode = exitCode;
}
