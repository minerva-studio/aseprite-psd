import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

import { initializeCanvas, readPsd } from "ag-psd";

const DEFAULT_INPUT = "path/to/fixture.psd";
const SCHEMA_VERSION = 1;

/**
 * Provides the raw RGBA image-data factory required by ag-psd in Node.js.
 * @param {number} width
 * @param {number} height
 * @returns {{ data: Uint8ClampedArray, width: number, height: number }}
 */
function createImageData(width, height) {
  return {
    data: new Uint8ClampedArray(width * height * 4),
    width,
    height,
  };
}

initializeCanvas(() => {
  throw new Error("Canvas output is not supported by the PSD oracle");
}, createImageData);

/** Runs the TypeScript ag-psd oracle and writes a normalized snapshot. */
async function main() {
  const args = parseArgs(process.argv.slice(2));
  const input = args.input ?? process.env.PSD2ASE_FIXTURE ?? DEFAULT_INPUT;
  const output = args.output ?? "target/probe/oracle-snapshot.json";
  const bytes = await readFile(input);
  const psd = readPsd(bytes, {
    useImageData: true,
    skipThumbnail: true,
  });
  const snapshot = buildSnapshot(bytes, psd);
  await mkdir(dirname(output), { recursive: true });
  await writeFile(output, `${JSON.stringify(snapshot, null, 2)}\n`);
  console.log(`wrote ${output}`);
}

/** Parses the two explicit path options used by the probe runner. */
function parseArgs(args) {
  const values = {};
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--input" || argument === "--output") {
      const value = args[index + 1];
      if (!value || value.startsWith("--")) {
        throw new Error(`${argument} requires a value`);
      }
      values[argument.slice(2)] = value;
      index += 1;
    } else {
      throw new Error(`unknown argument: ${argument}`);
    }
  }
  return values;
}

/** Builds the format-independent snapshot shared with the Rust probe. */
function buildSnapshot(bytes, psd) {
  const layers = [];
  const rootLayers = psd.children ?? [];
  rootLayers.forEach((layer, index) => collectLayer(layer, [String(index)], layers));
  const animations = psd.imageResources?.animations
    ? {
        frames: (psd.imageResources.animations.frames ?? []).map((frame) => ({
          id: frame.id,
          delay: frame.delay,
          dispose: frame.dispose ?? null,
        })),
        animation_sets: (psd.imageResources.animations.animations ?? []).map((animation) => ({
          id: animation.id,
          frames: animation.frames ?? [],
          repeats: animation.repeats ?? null,
          active_frame: animation.activeFrame ?? null,
        })),
      }
    : null;

  return {
    schema_version: SCHEMA_VERSION,
    source: {
      byte_length: bytes.byteLength,
      sha256: sha256Hex(bytes),
    },
    document: {
      width: psd.width,
      height: psd.height,
      channels: psd.channels ?? null,
      bits_per_channel: psd.bitsPerChannel ?? null,
      color_mode: colorModeName(psd.colorMode),
      root_layer_count: rootLayers.length,
      group_count: layers.filter((layer) => layer.kind === "group").length,
      pixel_layer_count: layers.filter((layer) => layer.kind === "pixel").length,
    },
    layers,
    animation: {
      resource_4000_exposed: animations !== null,
      animations,
      timeline_information_exposed: psd.imageResources?.timelineInformation != null,
    },
  };
}

/** Recursively converts one ag-psd layer into a normalized layer snapshot. */
function collectLayer(layer, path, snapshots) {
  const isGroup = Array.isArray(layer.children);
  snapshots.push({
    path: path.join("/"),
    id: layer.id ?? null,
    kind: isGroup ? "group" : "pixel",
    name: layer.name ?? "",
    top: layer.top ?? null,
    left: layer.left ?? null,
    bottom: layer.bottom ?? null,
    right: layer.right ?? null,
    opacity: layer.opacity ?? null,
    blend_mode: layer.blendMode == null ? null : normalizeName(String(layer.blendMode)),
    hidden: layer.hidden ?? null,
    pixel: isGroup ? null : pixelSnapshot(layer, path.join("/")),
    animation_frame_count: Array.isArray(layer.animationFrames)
      ? layer.animationFrames.length
      : null,
  });
  (layer.children ?? []).forEach((child, index) =>
    collectLayer(child, [...path, String(index)], snapshots),
  );
}

/** Converts a typed pixel buffer into dimensions and a SHA-256 digest. */
function pixelSnapshot(layer, path) {
  const value = layer.imageData;
  if (value == null) {
    throw new Error(`pixel layer has no RGBA8 data at ${path}: ${layer.name ?? "<unnamed>"}`);
  }
  const bytes = asBytes(value);
  const width = value.width ?? Math.max(0, Math.round((layer.right ?? 0) - (layer.left ?? 0)));
  const height =
    value.height ?? Math.max(0, Math.round((layer.bottom ?? 0) - (layer.top ?? 0)));
  const expected = width * height * 4;
  if (bytes.byteLength !== expected) {
    throw new Error(
      `pixel buffer length mismatch for layer ${layer.name ?? "<unnamed>"}: ` +
        `expected ${expected}, got ${bytes.byteLength}`,
    );
  }
  return {
    width,
    height,
    byte_length: bytes.byteLength,
    sha256: sha256Hex(bytes),
  };
}

/** Converts the supported ag-psd image-data representations into Uint8Array. */
function asBytes(value) {
  if (value instanceof Uint8Array) {
    return value;
  }
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  if (ArrayBuffer.isView(value?.data)) {
    return new Uint8Array(value.data.buffer, value.data.byteOffset, value.data.byteLength);
  }
  throw new Error("ag-psd returned an unsupported imageData representation");
}

/** Returns a lowercase SHA-256 hex digest for a byte buffer. */
function sha256Hex(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

/** Normalizes enum-like values to the lowercase probe representation. */
function normalizeName(value) {
  return value
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .replaceAll("_", " ")
    .toLowerCase();
}

/** Converts the PSD numeric color-mode value into the probe's stable name. */
function colorModeName(value) {
  const names = {
    0: "bitmap",
    1: "grayscale",
    2: "indexed",
    3: "rgb",
    4: "cmyk",
    7: "multichannel",
    8: "duotone",
    9: "lab",
  };
  return names[value] ?? (value == null ? null : normalizeName(String(value)));
}

await main();
