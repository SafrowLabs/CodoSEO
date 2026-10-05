#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");

const target = `${process.platform}-${process.arch}`;
const binaryName = process.platform === "win32" ? "codoseo.exe" : "codoseo";
const bundledBinary = path.join(__dirname, "..", "vendor", target, binaryName);
const requestedBinary = process.env.CODOSEO_BINARY;

function findOnPath(name) {
  const names = process.platform === "win32" ? [name, `${name}.exe`] : [name];
  for (const directory of (process.env.PATH || "").split(path.delimiter)) {
    for (const candidate of names) {
      const fullPath = path.join(directory, candidate);
      if (fs.existsSync(fullPath)) return fullPath;
    }
  }
  return undefined;
}

const binary = requestedBinary || (fs.existsSync(bundledBinary) ? bundledBinary : findOnPath("codoseo"));

if (!binary) {
  console.error("CodoSEO binary is not installed for this platform.");
  console.error("Reinstall the package, set CODOSEO_BINARY, or install CodoSEO with cargo:");
  console.error("  cargo install codoseo");
  process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
child.on("error", (error) => {
  console.error(`Unable to start CodoSEO: ${error.message}`);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
