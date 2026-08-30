import { readFile } from "node:fs/promises";

/** Compares two normalized probe snapshots and reports actionable mismatches. */
async function main() {
  const args = parseArgs(process.argv.slice(2));
  const rust = JSON.parse(await readFile(args.rust, "utf8"));
  const oracle = JSON.parse(await readFile(args.oracle, "utf8"));
  const mismatches = [];
  compareValues(stripAnimationFields(rust), stripAnimationFields(oracle), "$", mismatches);
  const animationMismatches = [];
  compareValues(
    extractAnimationFields(rust),
    extractAnimationFields(oracle),
    "$.animation",
    animationMismatches,
  );
  const normalizedMismatches = [];
  compareValues(
    rust.normalized_document,
    oracle.normalized_document,
    "$.normalized_document",
    normalizedMismatches,
  );

  if (mismatches.length === 0) {
    console.log("probe comparison: PASS (zero base metadata/layer/pixel mismatches)");
  } else {
    reportMismatches("probe comparison: FAIL", "base mismatches", mismatches, console.error);
    process.exitCode = 1;
  }

  if (animationMismatches.length === 0) {
    console.log("animation compatibility: PASS");
  } else {
    reportMismatches(
      "animation compatibility: FAIL",
      "animation mismatches",
      animationMismatches,
      console.error,
    );
    process.exitCode = 1;
  }

  if (normalizedMismatches.length === 0) {
    console.log("normalized document compatibility: PASS");
  } else {
    reportMismatches(
      "normalized document compatibility: FAIL",
      "normalized document mismatches",
      normalizedMismatches,
      console.error,
    );
    process.exitCode = 1;
  }
}

/** Removes animation-only fields before evaluating the base compatibility gate. */
function stripAnimationFields(value) {
  if (Array.isArray(value)) {
    return value.map(stripAnimationFields);
  }
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .filter(([key]) =>
          key !== "animation" &&
          key !== "animation_frame_count" &&
          key !== "normalized_document",
        )
        .map(([key, nested]) => [key, stripAnimationFields(nested)]),
    );
  }

  return value;
}

/** Extracts animation-only fields for the animation compatibility gate. */
function extractAnimationFields(snapshot) {
  return {
    animation: snapshot.animation,
    layer_animation_frame_counts: snapshot.layers.map((layer) => ({
      path: layer.path,
      animation_frame_count: layer.animation_frame_count,
    })),
  };
}

/** Reports the full mismatch count while limiting the displayed paths. */
function reportMismatches(prefix, description, mismatches, write) {
  const shown = mismatches.slice(0, 50);
  write(`${prefix} (${mismatches.length} ${description}; showing ${shown.length})`);
  shown.forEach((mismatch) => write(`- ${mismatch}`));
}

/** Parses the snapshot paths supplied by the probe runner. */
function parseArgs(args) {
  const values = {};
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument !== "--rust" && argument !== "--oracle") {
      throw new Error(`unknown argument: ${argument}`);
    }
    const value = args[index + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`${argument} requires a value`);
    }
    values[argument.slice(2)] = value;
    index += 1;
  }
  if (!values.rust || !values.oracle) {
    throw new Error("usage: node compare-probes.mjs --rust FILE --oracle FILE");
  }
  return values;
}

/** Recursively compares JSON values while retaining the first useful paths. */
function compareValues(left, right, path, mismatches) {
  if (left === right) {
    return;
  }
  if (typeof left !== typeof right || left === null || right === null) {
    mismatches.push(`${path}: Rust=${formatValue(left)} Oracle=${formatValue(right)}`);
    return;
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right)) {
      mismatches.push(`${path}: one value is an array and the other is not`);
      return;
    }
    if (left.length !== right.length) {
      mismatches.push(`${path}.length: Rust=${left.length} Oracle=${right.length}`);
    }
    const length = Math.min(left.length, right.length);
    for (let index = 0; index < length; index += 1) {
      compareValues(left[index], right[index], `${path}[${index}]`, mismatches);
    }
    return;
  }
  if (typeof left === "object") {
    const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
    for (const key of [...keys].sort()) {
      if (!(key in left) || !(key in right)) {
        mismatches.push(`${path}.${key}: field missing on one side`);
      } else {
        compareValues(left[key], right[key], `${path}.${key}`, mismatches);
      }
    }
    return;
  }
  mismatches.push(`${path}: Rust=${formatValue(left)} Oracle=${formatValue(right)}`);
}

/** Formats JSON values compactly for a mismatch report. */
function formatValue(value) {
  return JSON.stringify(value);
}

await main();
