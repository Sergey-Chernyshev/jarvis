"use strict";

const fs = require("node:fs");

const exactFiles = new Set(["src/power/mod.rs"]);
const directoryPrefixes = ["src/power/helper/"];

function hostRelative(fileName) {
  let normalized = String(fileName).replaceAll("\\", "/");
  const hostMarker = "/src-tauri/";
  const hostMarkerIndex = normalized.lastIndexOf(hostMarker);
  if (hostMarkerIndex >= 0) {
    return normalized.slice(hostMarkerIndex + hostMarker.length);
  }
  if (normalized.startsWith("src-tauri/")) {
    return normalized.slice("src-tauri/".length);
  }
  if (normalized.startsWith("./")) {
    normalized = normalized.slice(2);
  }
  return normalized;
}

function changedScopeWarnings(diagnostics) {
  const failures = [];
  for (const line of diagnostics.split("\n")) {
    if (!line) continue;

    let event;
    try {
      event = JSON.parse(line);
    } catch {
      continue;
    }
    if (event.reason !== "compiler-message") continue;

    const message = event.message;
    if (!message || message.level !== "warning") continue;

    const primaryFiles = (message.spans || [])
      .filter((span) => span.is_primary)
      .map((span) => hostRelative(span.file_name));
    if (
      primaryFiles.some(
        (file) =>
          exactFiles.has(file) ||
          directoryPrefixes.some((prefix) => file.startsWith(prefix)),
      )
    ) {
      failures.push(message.rendered || message.message);
    }
  }
  return failures;
}

function main(diagnosticsPath) {
  const failures = changedScopeWarnings(
    fs.readFileSync(diagnosticsPath, "utf8"),
  );
  if (failures.length > 0) {
    process.stderr.write(
      `host power-helper changed-scope clippy warnings:\n${failures.join("\n")}`,
    );
    process.exitCode = 1;
    return;
  }
  process.stdout.write("host power-helper changed-scope clippy: clean\n");
}

if (require.main === module) {
  main(process.argv[2]);
}

module.exports = { changedScopeWarnings, hostRelative };
