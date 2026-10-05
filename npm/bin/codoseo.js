#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");

const target = `${process.platform}-${process.arch}`;
const binaryName = process.platform === "win32" ? "codoseo.exe" : "codoseo";
const bundledBinary = path.join(__dirname, "..", "vendor", target, binaryName);
const requestedBinary = process.env.CODOSEO_BINARY;

if (requestedBinary && !path.isAbsolute(requestedBinary)) {
  console.error("CODOSEO_BINARY must be an absolute path to the native binary.");
  process.exit(1);
}

// PATH can resolve to this npm shim again, recursively spawning processes.
const binary = requestedBinary || (fs.existsSync(bundledBinary) ? bundledBinary : undefined);

if (binary && fs.existsSync(binary) && fs.realpathSync(binary) === fs.realpathSync(__filename)) {
  console.error("CODOSEO_BINARY must point to the native binary, not the npm launcher.");
  process.exit(1);
}

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
