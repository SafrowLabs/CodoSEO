const fs = require("node:fs");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

if (process.env.CODOSEO_SKIP_DOWNLOAD === "1") process.exit(0);

const targets = {
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc"
};

const npmTarget = `${process.platform}-${process.arch}`;
const rustTarget = targets[npmTarget];
const packageVersion = require("./package.json").version;
const archiveName = `codoseo-v${packageVersion}-${rustTarget}.tar.gz`;
const archiveUrl = `https://github.com/SafrowLabs/codoSEO/releases/download/v${packageVersion}/${archiveName}`;
const vendorDir = path.join(__dirname, "vendor", npmTarget);
const binaryName = process.platform === "win32" ? "codoseo.exe" : "codoseo";
const binaryPath = path.join(vendorDir, binaryName);

if (!rustTarget) {
  console.error(`CodoSEO does not have a published binary for ${npmTarget}.`);
  process.exit(1);
}

function download(url, destination) {
  return new Promise((resolve, reject) => {
    https.get(url, (response) => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
        response.resume();
        download(new URL(response.headers.location, url), destination).then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        response.resume();
        reject(new Error(`download returned HTTP ${response.statusCode}`));
        return;
      }
      const output = fs.createWriteStream(destination);
      response.pipe(output);
      output.on("finish", () => output.close(resolve));
      output.on("error", reject);
    }).on("error", reject);
  });
}

async function main() {
  const temporaryArchive = path.join(os.tmpdir(), `codoseo-${process.pid}.tar.gz`);
  fs.mkdirSync(vendorDir, { recursive: true });
  try {
    console.log(`Downloading CodoSEO ${packageVersion} for ${npmTarget}...`);
    await download(archiveUrl, temporaryArchive);
    const result = spawnSync("tar", ["-xzf", temporaryArchive, "-C", vendorDir], { stdio: "inherit" });
    if (result.error || result.status !== 0 || !fs.existsSync(binaryPath)) {
      throw result.error || new Error("downloaded archive did not contain the CodoSEO binary");
    }
    if (process.platform !== "win32") fs.chmodSync(binaryPath, 0o755);
  } catch (error) {
    console.error(`Could not install the CodoSEO binary: ${error.message}`);
    console.error(`Expected release asset: ${archiveUrl}`);
    process.exit(1);
  } finally {
    fs.rmSync(temporaryArchive, { force: true });
  }
}

main();
