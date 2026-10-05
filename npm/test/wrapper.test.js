const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const https = require("node:https");
const { EventEmitter } = require("node:events");
const { PassThrough } = require("node:stream");
const { createHash } = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { download, verifyChecksum } = require("../install.js");

const root = path.resolve(__dirname, "..");
function temporary(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "codoseo-test-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}
function run(script, args = [], env = {}) {
  return spawnSync(process.execPath, [path.join(root, script), ...args], {
    encoding: "utf8", timeout: 5000,
    env: { ...process.env, CODOSEO_BINARY: "", CODOSEO_SKIP_DOWNLOAD: "", ...env }
  });
}

test("explicit binary receives arguments and preserves exit status", () => {
  const result = run("bin/codoseo.js", ["-e", "console.log(process.argv[1]); process.exit(7)", "two words"], { CODOSEO_BINARY: process.execPath });
  assert.equal(result.status, 7);
  assert.equal(result.stdout.trim(), "two words");
});

test("missing native binary fails without following the npm shim on PATH", (t) => {
  const dir = temporary(t);
  fs.mkdirSync(path.join(dir, "bin"));
  fs.copyFileSync(path.join(root, "bin/codoseo.js"), path.join(dir, "bin/codoseo.js"));
  const result = spawnSync(process.execPath, [path.join(dir, "bin/codoseo.js")], {
    encoding: "utf8", timeout: 5000,
    env: { ...process.env, CODOSEO_BINARY: "", PATH: path.join(dir, "bin") }
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /binary is not installed/);
});

test("explicit npm launcher is rejected", () => {
  const result = run("bin/codoseo.js", [], { CODOSEO_BINARY: path.join(root, "bin/codoseo.js") });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /not the npm launcher/);
});

test("missing override reports spawn error", (t) => {
  const result = run("bin/codoseo.js", [], { CODOSEO_BINARY: path.join(temporary(t), "missing") });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Unable to start/);
});

for (const env of [{ CODOSEO_SKIP_DOWNLOAD: "1" }, { CODOSEO_BINARY: process.execPath }]) {
  test(`install skips network with ${Object.keys(env)[0]}`, () => {
    const result = run("install.js", [], env);
    assert.equal(result.status, 0);
    assert.equal(result.stdout, "");
  });
}

test("checksum accepts matching assets and rejects corruption or wrong filenames", async (t) => {
  const dir = temporary(t);
  const archive = path.join(dir, "binary.tar.gz");
  const checksum = `${archive}.sha256`;
  const data = Buffer.from("release archive");
  const digest = createHash("sha256").update(data).digest("hex");
  fs.writeFileSync(archive, data);
  fs.writeFileSync(checksum, `${digest}  binary.tar.gz\n`);
  await verifyChecksum(archive, checksum, "binary.tar.gz");
  await assert.rejects(verifyChecksum(archive, checksum, "other.tar.gz"), /invalid release checksum/);
  fs.appendFileSync(archive, "corrupted");
  await assert.rejects(verifyChecksum(archive, checksum, "binary.tar.gz"), /checksum mismatch/);
  fs.writeFileSync(checksum, "not a checksum");
  await assert.rejects(verifyChecksum(archive, checksum, "binary.tar.gz"), /invalid release checksum/);
});

test("download rejects insecure URLs and excessive redirects", async (t) => {
  const dest = path.join(temporary(t), "archive");
  await assert.rejects(download("http://example.com/archive", dest), /requires HTTPS/);
  await assert.rejects(download("https://example.com/archive", dest, 6), /too many/);
});

test("download handles redirects, HTTP errors, and interrupted streams", async (t) => {
  const dir = temporary(t);
  let mode = "redirect";
  const original = https.get;
  t.after(() => { https.get = original; });
  https.get = (url, callback) => {
    const request = new EventEmitter();
    request.setTimeout = () => {};
    process.nextTick(() => {
      const response = new PassThrough();
      response.headers = {};
      response.statusCode = 200;
      if (mode === "redirect") {
        response.statusCode = 302;
        response.headers.location = "https://example.com/asset";
        mode = "success";
      } else if (mode === "http-error") response.statusCode = 404;
      callback(response);
      if (mode === "interrupted") response.destroy(new Error("interrupted transfer"));
      else response.end("archive bytes");
    });
    return request;
  };
  const dest = path.join(dir, "success");
  await download("https://example.com/start", dest);
  assert.equal(fs.readFileSync(dest, "utf8"), "archive bytes");
  mode = "http-error";
  await assert.rejects(download("https://example.com/404", path.join(dir, "404")), /HTTP 404/);
  mode = "interrupted";
  await assert.rejects(download("https://example.com/interrupted", path.join(dir, "partial")), /interrupted transfer/);
});

test("installer verifies and extracts release assets, and cleans temporary files", (t) => {
  const dir = temporary(t);
  const pkg = path.join(dir, "package");
  const temp = path.join(dir, "temp");
  const staging = path.join(dir, "staging");
  for (const destination of [pkg, temp, staging]) fs.mkdirSync(destination);
  for (const file of ["install.js", "package.json"]) fs.copyFileSync(path.join(root, file), path.join(pkg, file));
  const binary = process.platform === "win32" ? "codoseo.exe" : "codoseo";
  fs.writeFileSync(path.join(staging, binary), "native binary fixture");
  const archive = path.join(dir, "fixture.tar.gz");
  const packed = spawnSync("tar", ["-czf", archive, "-C", staging, binary]);
  assert.equal(packed.status, 0, packed.stderr?.toString());
  const preloader = path.join(dir, "mock-download.cjs");
  fs.writeFileSync(preloader, `
    const fs = require('node:fs');
    const { Readable } = require('node:stream');
    const { EventEmitter } = require('node:events');
    const { createHash } = require('node:crypto');
    require('node:https').get = (url, callback) => {
      const request = new EventEmitter();
      request.setTimeout = () => {};
      process.nextTick(() => {
        const archive = fs.readFileSync(process.env.TEST_ARCHIVE);
        const name = new URL(url).pathname.split('/').pop();
        const digest = process.env.TEST_CORRUPT ? '0'.repeat(64) : createHash('sha256').update(archive).digest('hex');
        const data = name.endsWith('.sha256') ? Buffer.from(digest + '  ' + name.slice(0, -7) + '\\n') : archive;
        const response = Readable.from([data]);
        response.statusCode = 200;
        response.headers = {};
        callback(response);
      });
      return request;
    };
  `);
  function install(corrupt) {
    return spawnSync(process.execPath, ["--require", preloader, path.join(pkg, "install.js")], {
      encoding: "utf8", timeout: 5000,
      env: { ...process.env, CODOSEO_BINARY: "", CODOSEO_SKIP_DOWNLOAD: "",
        TEST_ARCHIVE: archive, TEST_CORRUPT: corrupt ? "1" : "", TMPDIR: temp, TEMP: temp, TMP: temp }
    });
  }
  const failed = install(true);
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /checksum mismatch/);
  assert.equal(fs.existsSync(path.join(pkg, "vendor")), false);
  assert.deepEqual(fs.readdirSync(temp), []);
  const passed = install(false);
  assert.equal(passed.status, 0, passed.stderr);
  const installed = path.join(pkg, "vendor", `${process.platform}-${process.arch}`, binary);
  assert.equal(fs.readFileSync(installed, "utf8"), "native binary fixture");
  if (process.platform !== "win32") assert.equal(fs.statSync(installed).mode & 0o777, 0o755);
  assert.deepEqual(fs.readdirSync(temp), []);
});


test("bare command overrides cannot recurse through PATH", () => {
  const result = run("bin/codoseo.js", [], { CODOSEO_BINARY: "codoseo" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /absolute path/);
});
