import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

import { initializeCanvas, readPsd } from "ag-psd";
import { createReader } from "ag-psd/dist/psdReader.js";
import { resourceHandlersMap } from "ag-psd/dist/imageResources.js";

const DEFAULT_INPUT = "path/to/fixture.psd";
const SCHEMA_VERSION = 4;

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
  const animation = buildAnimationSnapshot(bytes, rootLayers);
  const normalizedDocument = buildNormalizedDocumentSnapshot(animation, rootLayers);

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
    animation,
    normalized_document: normalizedDocument,
  };
}

/** Builds the complete normalized-model animation view, including static fallback frames. */
function buildNormalizedDocumentSnapshot(animation, rootLayers) {
  const layers = [];
  rootLayers.forEach((layer, index) => flattenLayer(layer, [String(index)], [], layers));
  if (animation.frames.length === 0) {
    return {
      frames: [{ index: 0, source_id: null, duration_ms: null, dispose: null }],
      loop_mode: null,
      active_frame: null,
      resource_ids: [],
      layer_states: layers.map((layer) => ({
        layer_id: layer.id,
        path: layer.path,
        frames: [{
          frame_index: 0,
          record_present: false,
          enabled: !(layer.hidden ?? false),
          explicit_enable: false,
          offset: null,
          reference_point: null,
          opacity: null,
        }],
      })),
    };
  }
  return {
    frames: animation.frames.map((frame, index) => ({
      index,
      source_id: frame.id,
      duration_ms: frame.duration_ms,
      dispose: frame.dispose,
    })),
    loop_mode: animation.loop_mode,
    active_frame: animation.active_frame,
    resource_ids: animation.resource_ids,
    layer_states: animation.layer_states.map((layer) => ({
      layer_id: layer.layer_id,
      path: layer.path,
      frames: layer.frames.map((frame, frameIndex) => ({
        frame_index: frameIndex,
        record_present: frame.record_present,
        enabled: frame.enabled,
        explicit_enable: frame.explicit_enable,
        offset: frame.offset,
        reference_point: frame.reference_point,
        opacity: frame.opacity,
      })),
    })),
  };
}

/** Builds the normalized Photoshop animation snapshot used by the stage gate. */
function buildAnimationSnapshot(bytes, rootLayers) {
  const resource = readAnimationResource(bytes);
  const layers = [];
  rootLayers.forEach((layer, index) => flattenLayer(layer, [String(index)], [], layers));
  const hasLayerAnimation = layers.some((layer) => Array.isArray(layer.animationFrames));
  if (resource == null && !hasLayerAnimation) {
    return emptyAnimationSnapshot();
  }
  if (resource == null) {
    throw new Error("layer animation metadata exists without a 4000/4003 frame catalog");
  }

  const frames = resource.frames.map((frame) => ({
    id: frame.id,
    duration_ms: Math.round(frame.delay * 1000),
    dispose: null,
  }));
  if (resource.animation_sets.length > 1) {
    throw new Error("multiple animation sets are ambiguous");
  }
  const animationSet = resource.animation_sets[0] ?? null;
  const loopMode = animationSet == null
    ? null
    : animationSet.repeats == null
      ? null
      : animationSet.repeats === 0
        ? "infinite"
        : "finite:" + animationSet.repeats;
  const activeFrame = animationSet?.active_frame ?? animationSet?.activeFrame ?? null;
  if (animationSet != null &&
      JSON.stringify(animationSet.frames) !== JSON.stringify(frames.map((frame) => frame.id))) {
    throw new Error("animation set frame order differs from FrIn");
  }

  const layerStates = layers.map((layer) => {
    const hasAnimationRecords = Array.isArray(layer.animationFrames) &&
      layer.animationFrames.length > 0;
    let previousEnabled = !layer.hidden;
    return {
      layer_id: layer.id ?? 0,
      path: layer.path,
      frames: frames.map((frame) => {
        const record = layer.animationFrames?.find((item) => item.frames?.includes(frame.id));
        const explicitEnable = record?.enable !== undefined;
        if (layer.isGroup && !layer.isContainerGroup && hasAnimationRecords) {
          previousEnabled = record?.enable ?? false;
        } else if (explicitEnable) {
          previousEnabled = record.enable;
        } else if (record == null && hasAnimationRecords) {
          previousEnabled = false;
        }
        return {
          frame_id: frame.id,
          record_present: record != null,
          enabled: previousEnabled,
          explicit_enable: explicitEnable,
          offset: record?.offset ?? null,
          reference_point: record?.referencePoint ?? null,
          opacity: record?.opacity ?? null,
        };
      }),
    };
  });
  const statesById = new Map(layerStates.map((state) => [state.layer_id, state]));
  const visiblePixelLayers = frames.map((frame) => ({
    frame_id: frame.id,
    layer_ids: layers
      .filter((layer) => !layer.isGroup)
      .filter((layer) => {
        const state = statesById.get(layer.id);
        return state.frames.find((item) => item.frame_id === frame.id)?.enabled &&
          layer.ancestorIds.every((ancestorId) =>
            statesById.get(ancestorId)?.frames.find((item) => item.frame_id === frame.id)?.enabled,
          );
      })
      .map((layer) => layer.id),
  }));
  const flag = layers.find((layer) => layer.animationFrameFlags)?.animationFrameFlags;
  return {
    resource_ids: resource.resource_ids,
    frames,
    loop_mode: loopMode,
    active_frame: activeFrame,
    layer_states: layerStates,
    visible_pixel_layers: visiblePixelLayers,
    frame_flags: flag == null ? null : {
      propagate_frame_one: flag.propagateFrameOne ?? false,
      unify_layer_position: flag.unifyLayerPosition ?? false,
      unify_layer_style: flag.unifyLayerStyle ?? false,
      unify_layer_visibility: flag.unifyLayerVisibility ?? false,
    },
  };
}

/** Returns the empty normalized animation result for a PSD without animation. */
function emptyAnimationSnapshot() {
  return {
    resource_ids: [],
    frames: [],
    loop_mode: null,
    active_frame: null,
    layer_states: [],
    visible_pixel_layers: [],
    frame_flags: null,
  };
}

/** Flattens the ag-psd tree while retaining group ancestry for visibility. */
function flattenLayer(layer, path, ancestorIds, output) {
  output.push({
    path: path.join("/"),
    id: layer.id ?? 0,
    hidden: layer.hidden ?? false,
    isGroup: Array.isArray(layer.children),
    isContainerGroup: Array.isArray(layer.children) &&
      layer.children.length > 0 && layer.children.every((child) => Array.isArray(child.children)),
    ancestorIds,
    animationFrames: layer.animationFrames,
    animationFrameFlags: layer.animationFrameFlags,
  });
  const nextAncestors = Array.isArray(layer.children) && layer.id != null
    ? [...ancestorIds, layer.id]
    : ancestorIds;
  (layer.children ?? []).forEach((child, index) =>
    flattenLayer(child, [...path, String(index)], nextAncestors, output),
  );
}

/** Reads the animation descriptor from both Photoshop resource IDs 4000 and 4003. */
function readAnimationResource(bytes) {
  let offset = 26;
  const colorLength = readUint32(bytes, offset);
  offset += 4 + colorLength;
  const resourceLength = readUint32(bytes, offset);
  offset += 4;
  const end = offset + resourceLength;
  let result = null;
  const resourceIds = [];
  while (offset < end) {
    const signature = ascii(bytes, offset, 4);
    offset += 4;
    if (signature !== "8BIM") throw new Error("invalid image resource signature at " + (offset - 4));
    const id = readUint16(bytes, offset);
    offset += 2;
    const nameLength = bytes[offset];
    offset += 1 + nameLength + ((1 + nameLength) % 2);
    const length = readUint32(bytes, offset);
    offset += 4;
    const data = bytes.subarray(offset, offset + length);
    offset += length + (length % 2);
    if ((id === 4000 || id === 4003) && ascii(data, 0, 4) === "mani") {
      const reader = createReader(data.buffer, data.byteOffset, data.byteLength);
      const target = {};
      resourceHandlersMap[4000].read(reader, target, () => data.byteLength - reader.offset);
      if (result != null) throw new Error("multiple animation resources are ambiguous");
      result = {
        resource_ids: [id],
        frames: target.animations?.frames ?? [],
        animation_sets: target.animations?.animations ?? [],
      };
      resourceIds.push(id);
    }
  }
  return result == null ? null : { ...result, resource_ids: resourceIds };
}

/** Reads a checked big-endian uint16 from the PSD byte buffer. */
function readUint16(bytes, offset) {
  if (offset + 2 > bytes.length) throw new Error("truncated uint16 at " + offset);
  return bytes[offset] * 256 + bytes[offset + 1];
}

/** Reads a checked big-endian uint32 from the PSD byte buffer. */
function readUint32(bytes, offset) {
  if (offset + 4 > bytes.length) throw new Error("truncated uint32 at " + offset);
  return bytes[offset] * 0x1000000 + bytes[offset + 1] * 0x10000 +
    bytes[offset + 2] * 0x100 + bytes[offset + 3];
}

/** Converts a byte range to its ASCII representation. */
function ascii(bytes, offset, length) {
  if (offset + length > bytes.length) throw new Error("truncated ASCII field at " + offset);
  return String.fromCharCode(...bytes.subarray(offset, offset + length));
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
    left: layer.left ?? 0,
    top: layer.top ?? 0,
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
