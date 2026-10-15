import { createHash } from "node:crypto";
import { execSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { brotliCompress, constants } from "node:zlib";

const compress = promisify(brotliCompress);
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = new Set(process.argv.slice(2));
const version = readFileSync(join(root, "Cargo.toml"), "utf8").match(/\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"/)[1];
const release = join(root, "target", "release");
const resources = join(root, "resources");
const outDir = join(root, "release");
const MAGIC = Buffer.from("PRISMPK1", "ascii");

const run = (cmd, cwd = root) => execSync(cmd, { cwd, stdio: "inherit", env: process.env });

const fullBuild = !args.has("--skip-build") && !args.has("--payload-only");
if (fullBuild || !existsSync(join(resources, "cores", "xray.exe"))) run("node scripts/fetch-cores.mjs");

if (!args.has("--skip-build")) {
  run(args.has("--payload-only") ? "cargo build --release -p prism" : "cargo build --release -p prism -p prism-setup");
}

const files = [
  ["prism.exe", join(release, "prism.exe")],
  ["cores/xray.exe", join(resources, "cores", "xray.exe")],
  ["cores/sing-box.exe", join(resources, "cores", "sing-box.exe")],
  ["cores/versions.json", join(resources, "cores", "versions.json")],
  ["geo/geoip.dat", join(resources, "geo", "geoip.dat")],
  ["geo/geosite.dat", join(resources, "geo", "geosite.dat")],
  ["routing/DEFAULT.DEEPLINK", join(resources, "routing", "DEFAULT.DEEPLINK")],
];
for (const [, src] of files) if (!existsSync(src)) throw new Error(`missing ${src}`);

const started = Date.now();
process.stdout.write("compressing payload ... ");
const packed = await Promise.all(
  files.map(async ([path, src]) => {
    const data = readFileSync(src);
    const body = await compress(data, {
      params: {
        [constants.BROTLI_PARAM_QUALITY]: 11,
        [constants.BROTLI_PARAM_LGWIN]: 24,
        [constants.BROTLI_PARAM_SIZE_HINT]: data.length,
      },
    });
    return { path, size: data.length, sha256: createHash("sha256").update(data).digest("hex"), body };
  }),
);
console.log(`${((Date.now() - started) / 1000).toFixed(1)}s`);

const payloadOnly = args.has("--payload-only");
const setupExe = payloadOnly ? Buffer.alloc(0) : readFileSync(join(release, "prism-setup.exe"));
const chunks = [setupExe];
let offset = setupExe.length;
const entries = packed.map(({ path, size, sha256, body }) => {
  const entry = { path, offset, len: body.length, size, sha256 };
  chunks.push(body);
  offset += body.length;
  return entry;
});
const manifest = Buffer.from(
  JSON.stringify({ name: "Prism", version, exe: "prism.exe", base: setupExe.length, files: entries }),
  "utf8",
);
const trailer = Buffer.alloc(24);
MAGIC.copy(trailer, 0);
trailer.writeBigUInt64LE(BigInt(offset), 8);
trailer.writeBigUInt64LE(BigInt(manifest.length), 16);
chunks.push(manifest, trailer);

mkdirSync(outDir, { recursive: true });
const out = join(outDir, payloadOnly ? "prism-dev.pkg" : `Prism-Setup-${version}.exe`);
writeFileSync(out, Buffer.concat(chunks));
const raw = packed.reduce((n, f) => n + f.size, 0);
console.log(
  `${out}\n  ${(statSync(out).size / 2 ** 20).toFixed(1)} MB (app files ${(raw / 2 ** 20).toFixed(1)} MB unpacked)`,
);
