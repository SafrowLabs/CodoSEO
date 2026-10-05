const fs = require("node:fs");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const { createHash } = require("node:crypto");
const { pipeline } = require("node:stream/promises");
const { spawnSync } = require("node:child_process");

// The release workflow builds exactly these targets. Linux binaries are static musl builds, so they
// run on glibc and Alpine alike.
const targets = {
  "linux-x64": "x86_64-unknown-linux-musl",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc"
};

function download(url, destination, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (new URL(url).protocol !== "https:") return reject(new Error("download requires HTTPS"));
    if (redirects > 5) return reject(new Error("too many download redirects"));
    const request = https.get(url, (response) => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
        response.resume();
        download(new URL(response.headers.location, url), destination, redirects + 1).then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        response.resume();
        reject(new Error(`download returned HTTP ${response.statusCode}`));
        return;
      }
      const output = fs.createWriteStream(destination, { flags: "wx" });
      // pipeline can reject before an asynchronously opening file has closed.
      // Wait for close so installer cleanup is safe on Windows as well.
      const closed = new Promise((done) => output.once("close", done));
      pipeline(response, output).then(resolve, async (error) => {
        await closed;
        reject(error);
      });
    });
    request.setTimeout(30_000, () => request.destroy(new Error("download timed out")));
    request.on("error", reject);
  });
}

async function verifyChecksum(archive, checksumFile, archiveName) {
  const entry = fs.readFileSync(checksumFile, "utf8").trim().match(/^([a-f0-9]{64})\s+\*?([^\r\n]+)$/i);
  if (!entry || entry[2] !== archiveName) throw new Error("invalid release checksum file");
  const hash = createHash("sha256");
  for await (const chunk of fs.createReadStream(archive)) hash.update(chunk);
  if (hash.digest("hex") !== entry[1].toLowerCase()) throw new Error("release checksum mismatch");
}

async function main() {
  if (process.env.CODOSEO_SKIP_DOWNLOAD === "1" || process.env.CODOSEO_BINARY) return;
  const npmTarget = `${process.platform}-${process.arch}`;
  const rustTarget = targets[npmTarget];
  if (!rustTarget) throw new Error(`CodoSEO does not have a published binary for ${npmTarget}.`);
  const packageVersion = require("./package.json").version;
  const archiveName = `codoseo-v${packageVersion}-${rustTarget}.tar.gz`;
  const archiveUrl = `https://github.com/SafrowLabs/codoSEO/releases/download/v${packageVersion}/${archiveName}`;
  const vendorDir = path.join(__dirname, "vendor", npmTarget);
  const binaryName = process.platform === "win32" ? "codoseo.exe" : "codoseo";
  const temporaryDir = fs.mkdtempSync(path.join(os.tmpdir(), "codoseo-"));
  const temporaryArchive = path.join(temporaryDir, archiveName);
  try {
    console.log(`Downloading CodoSEO ${packageVersion} for ${npmTarget}...`);
    await download(archiveUrl, temporaryArchive);
    await download(`${archiveUrl}.sha256`, `${temporaryArchive}.sha256`);
    await verifyChecksum(temporaryArchive, `${temporaryArchive}.sha256`, archiveName);
    // Extract only the expected binary into a private directory before installing it.
    const result = spawnSync("tar", ["-xzf", temporaryArchive, "-C", temporaryDir, binaryName], { stdio: "inherit" });
    const extracted = path.join(temporaryDir, binaryName);
    if (result.error || result.status !== 0 || !fs.existsSync(extracted) || !fs.lstatSync(extracted).isFile()) {
      throw result.error || new Error("downloaded archive did not contain a regular CodoSEO binary");
    }
    fs.mkdirSync(vendorDir, { recursive: true });
    fs.copyFileSync(extracted, path.join(vendorDir, binaryName));
    if (process.platform !== "win32") fs.chmodSync(path.join(vendorDir, binaryName), 0o755);
  } catch (error) {
    throw new Error(`${error.message}\nExpected release asset: ${archiveUrl}`);
  } finally {
    fs.rmSync(temporaryDir, { recursive: true, force: true });
  }
}

if (require.main === module) {
  main().catch((error) => {
    console.error(`Could not install the CodoSEO binary: ${error.message}`);
    process.exitCode = 1;
  });
}

module.exports = { download, verifyChecksum, targets };
