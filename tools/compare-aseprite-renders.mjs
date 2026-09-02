import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { inflateSync } from "node:zlib";

/** Compares frame-indexed RGBA PNG directories and writes a visual diff report. */
async function main() {
  const args = parseArgs(process.argv.slice(2));
  const leftFiles = await frameFiles(args.left);
  const rightFiles = await frameFiles(args.right);
  const leftNames = leftFiles.map((file) => file.name);
  const rightNames = rightFiles.map((file) => file.name);
  if (JSON.stringify(leftNames) !== JSON.stringify(rightNames)) {
    throw new Error(`frame sets differ: left=${leftNames.join(",")} right=${rightNames.join(",")}`);
  }

  const frames = [];
  for (let index = 0; index < leftFiles.length; index += 1) {
    const left = decodePng(await readFile(leftFiles[index].path));
    const right = decodePng(await readFile(rightFiles[index].path));
    frames.push(compareFrame(left, right, leftFiles[index].name));
  }

  const report = {
    schema_version: 1,
    left: args.left,
    right: args.right,
    frame_count: frames.length,
    frames,
    totals: sumFrames(frames),
    passed: frames.every((frame) => frame.visible_differences === 0 && frame.alpha_differences === 0),
  };
  const outputDirectory = args.output.slice(0, Math.max(args.output.lastIndexOf("/"), args.output.lastIndexOf("\\")));
  if (outputDirectory) {
    await mkdir(outputDirectory, { recursive: true });
  }
  await writeFile(args.output, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  console.log(
    `render comparison: ${report.passed ? "PASS" : "FAIL"} ` +
      `(frames=${report.frame_count}, visible=${report.totals.visible_differences}, ` +
      `alpha=${report.totals.alpha_differences}, ` +
      `transparent-rgb-only=${report.totals.transparent_rgb_only_differences})`,
  );
  if (!report.passed) {
    process.exitCode = 1;
  }
}

/** Returns sorted frame PNG paths from one render directory. */
async function frameFiles(directory) {
  const names = (await readdir(directory))
    .filter((name) => /^frame-\d+\.png$/i.test(name))
    .sort((left, right) => frameNumber(left) - frameNumber(right));
  if (names.length === 0) {
    throw new Error(`no frame-N.png files found in ${directory}`);
  }
  return names.map((name) => ({ name, path: `${directory}/${name}` }));
}

/** Extracts the numeric frame index from a frame filename. */
function frameNumber(name) {
  return Number.parseInt(name.match(/^frame-(\d+)\.png$/i)[1], 10);
}

/** Compares two decoded RGBA frames and separates visible, alpha, and transparent RGB changes. */
function compareFrame(left, right, name) {
  if (left.width !== right.width || left.height !== right.height) {
    throw new Error(
      `${name} dimensions differ: left=${left.width}x${left.height} right=${right.width}x${right.height}`,
    );
  }
  let visibleDifferences = 0;
  let alphaDifferences = 0;
  let transparentRgbOnlyDifferences = 0;
  for (let offset = 0; offset < left.data.length; offset += 4) {
    const sameRgb =
      left.data[offset] === right.data[offset] &&
      left.data[offset + 1] === right.data[offset + 1] &&
      left.data[offset + 2] === right.data[offset + 2];
    const sameAlpha = left.data[offset + 3] === right.data[offset + 3];
    if (sameRgb && sameAlpha) {
      continue;
    }
    if (!sameAlpha) {
      alphaDifferences += 1;
    }
    if (left.data[offset + 3] !== 0 || right.data[offset + 3] !== 0) {
      visibleDifferences += 1;
    } else if (!sameRgb) {
      transparentRgbOnlyDifferences += 1;
    }
  }
  return {
    frame: frameNumber(name),
    name,
    width: left.width,
    height: left.height,
    visible_differences: visibleDifferences,
    alpha_differences: alphaDifferences,
    transparent_rgb_only_differences: transparentRgbOnlyDifferences,
  };
}

/** Sums per-frame difference counts into one report total. */
function sumFrames(frames) {
  return frames.reduce(
    (totals, frame) => ({
      visible_differences: totals.visible_differences + frame.visible_differences,
      alpha_differences: totals.alpha_differences + frame.alpha_differences,
      transparent_rgb_only_differences:
        totals.transparent_rgb_only_differences + frame.transparent_rgb_only_differences,
    }),
    { visible_differences: 0, alpha_differences: 0, transparent_rgb_only_differences: 0 },
  );
}

/** Decodes the non-interlaced 8-bit RGB/RGBA PNGs emitted by Aseprite. */
function decodePng(bytes) {
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  if (!bytes.subarray(0, 8).equals(signature)) {
    throw new Error("input is not a PNG");
  }
  let offset = 8;
  let width;
  let height;
  let colorType;
  let interlace;
  const compressed = [];
  while (offset < bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString("ascii", offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    offset += 12 + length;
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      const bitDepth = data[8];
      colorType = data[9];
      interlace = data[12];
      if (bitDepth !== 8 || ![2, 6].includes(colorType) || interlace !== 0) {
        throw new Error("only non-interlaced 8-bit RGB/RGBA PNGs are supported");
      }
    } else if (type === "IDAT") {
      compressed.push(data);
    } else if (type === "IEND") {
      break;
    }
  }
  if (!width || !height || !compressed.length) {
    throw new Error("PNG is missing IHDR or IDAT data");
  }
  const channels = colorType === 6 ? 4 : 3;
  const stride = width * channels;
  const raw = inflateSync(Buffer.concat(compressed));
  if (raw.length !== height * (stride + 1)) {
    throw new Error("PNG scanline data length does not match its dimensions");
  }
  const scanlines = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y += 1) {
    const filter = raw[y * (stride + 1)];
    const sourceStart = y * (stride + 1) + 1;
    const targetStart = y * stride;
    for (let x = 0; x < stride; x += 1) {
      const left = x >= channels ? scanlines[targetStart + x - channels] : 0;
      const above = y > 0 ? scanlines[targetStart - stride + x] : 0;
      const upperLeft = y > 0 && x >= channels ? scanlines[targetStart - stride + x - channels] : 0;
      const value = raw[sourceStart + x];
      scanlines[targetStart + x] = unfilter(filter, value, left, above, upperLeft);
    }
  }
  const rgba = Buffer.alloc(width * height * 4);
  for (let index = 0; index < width * height; index += 1) {
    const source = index * channels;
    const target = index * 4;
    rgba[target] = scanlines[source];
    rgba[target + 1] = scanlines[source + 1];
    rgba[target + 2] = scanlines[source + 2];
    rgba[target + 3] = channels === 4 ? scanlines[source + 3] : 255;
  }
  return { width, height, data: rgba };
}

/** Reverses one PNG scanline filter byte. */
function unfilter(filter, value, left, above, upperLeft) {
  if (filter === 0) return value;
  if (filter === 1) return (value + left) & 0xff;
  if (filter === 2) return (value + above) & 0xff;
  if (filter === 3) return (value + Math.floor((left + above) / 2)) & 0xff;
  if (filter === 4) return (value + paeth(left, above, upperLeft)) & 0xff;
  throw new Error(`unsupported PNG filter: ${filter}`);
}

/** Calculates the PNG Paeth predictor. */
function paeth(left, above, upperLeft) {
  const estimate = left + above - upperLeft;
  const leftDistance = Math.abs(estimate - left);
  const aboveDistance = Math.abs(estimate - above);
  const upperLeftDistance = Math.abs(estimate - upperLeft);
  if (leftDistance <= aboveDistance && leftDistance <= upperLeftDistance) return left;
  if (aboveDistance <= upperLeftDistance) return above;
  return upperLeft;
}

/** Parses the comparator command line. */
function parseArgs(args) {
  const values = {};
  for (let index = 0; index < args.length; index += 2) {
    const option = args[index];
    const value = args[index + 1];
    if (!["--left", "--right", "--output"].includes(option) || !value || value.startsWith("--")) {
      throw new Error("usage: node compare-aseprite-renders.mjs --left DIR --right DIR --output FILE");
    }
    values[option.slice(2)] = value;
  }
  if (!values.left || !values.right || !values.output || args.length % 2 !== 0) {
    throw new Error("usage: node compare-aseprite-renders.mjs --left DIR --right DIR --output FILE");
  }
  return values;
}

await main();
