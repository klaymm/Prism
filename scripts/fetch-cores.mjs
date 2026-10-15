import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync, rmSync, readdirSync, statSync, copyFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const resources = join(root, "resources");
const coresDir = join(resources, "cores");
const geoDir = join(resources, "geo");
const routingDir = join(resources, "routing");
const ROUTING_URL =
  "https://raw.githubusercontent.com/hydraponique/roscomvpn-routing/refs/heads/main/HAPP/DEFAULT.DEEPLINK";

const ghHeaders = {
  "User-Agent": "prism-build",
  Accept: "application/vnd.github+json",
  ...(process.env.GITHUB_TOKEN ? { Authorization: `Bearer ${process.env.GITHUB_TOKEN}` } : {}),
};

async function getJson(url) {
  const res = await fetch(url, { headers: ghHeaders });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return res.json();
}

async function download(url) {
  const res = await fetch(url, { headers: { "User-Agent": "prism-build" } });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return Buffer.from(await res.arrayBuffer());
}

const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

async function expectedHash(release, asset) {
  if (asset.digest?.startsWith("sha256:")) return asset.digest.slice(7);
  const dgst = release.assets.find((a) => a.name === `${asset.name}.dgst`);
  if (!dgst) return null;
  const text = (await download(dgst.browser_download_url)).toString("utf8");
  return text.match(/SHA2?-256=\s*([0-9a-f]{64})/i)?.[1] ?? null;
}

function findFile(dir, name) {
  for (const entry of readdirSync(dir)) {
    const p = join(dir, entry);
    if (statSync(p).isDirectory()) {
      const found = findFile(p, name);
      if (found) return found;
    } else if (entry.toLowerCase() === name) {
      return p;
    }
  }
  return null;
}

async function fetchCore({ repo, match, exe }) {
  const release = await getJson(`https://api.github.com/repos/${repo}/releases/latest`);
  const asset = release.assets.find((a) => match.test(a.name));
  if (!asset) throw new Error(`${repo}: no matching asset in ${release.tag_name}`);
  process.stdout.write(`${repo} ${release.tag_name}: ${asset.name} ... `);
  const zip = await download(asset.browser_download_url);
  const want = await expectedHash(release, asset);
  if (!want) throw new Error(`${asset.name}: no checksum published, refusing to use it`);
  if (sha256(zip) !== want.toLowerCase()) throw new Error(`${asset.name}: sha256 mismatch`);

  const work = join(tmpdir(), `prism-${Date.now()}-${exe}`);
  mkdirSync(work, { recursive: true });
  try {
    const zipPath = join(work, asset.name);
    writeFileSync(zipPath, zip);
    const tar = join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
    execFileSync(tar, ["-xf", zipPath, "-C", work]);
    const file = findFile(work, exe);
    if (!file) throw new Error(`${asset.name}: ${exe} not found in archive`);
    copyFileSync(file, join(coresDir, exe));
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
  console.log("ok");
  return release.tag_name;
}

async function fetchRouting() {
  process.stdout.write("routing profile ... ");
  const raw = (await download(ROUTING_URL)).toString("utf8").trim();
  const b64 = raw.replace(/^happ:\/\/routing\/(onadd|add)\//, "");
  const profile = JSON.parse(Buffer.from(b64, "base64").toString("utf8"));
  writeFileSync(join(routingDir, "DEFAULT.DEEPLINK"), raw);
  writeFileSync(join(geoDir, "geoip.dat"), await download(profile.Geoipurl));
  writeFileSync(join(geoDir, "geosite.dat"), await download(profile.Geositeurl));
  console.log(`ok (LastUpdated ${profile.LastUpdated})`);
}

for (const d of [coresDir, geoDir, routingDir]) mkdirSync(d, { recursive: true });

const versions = {
  xray: await fetchCore({ repo: "XTLS/Xray-core", match: /^Xray-windows-64\.zip$/, exe: "xray.exe" }),
  singbox: await fetchCore({ repo: "SagerNet/sing-box", match: /^sing-box-[\d.]+-windows-amd64\.zip$/, exe: "sing-box.exe" }),
};
writeFileSync(join(coresDir, "versions.json"), JSON.stringify(versions, null, 2));
await fetchRouting();
if (!existsSync(join(coresDir, "xray.exe"))) process.exit(1);
