// Signs a release for Familiar's in-app updates (run by .github/workflows/release.yml's publish job; no dependencies).
//
//   FAMILIAR_UPDATE_KEY=<minisign secret key file> node scripts/sign-release.mjs \
//     --version 0.1.0-alpha.3 --dir out --pubkey apps/native/dist/familiar-update.pub [--notes notes.md]
//
// For every artifact in --dir (the installer, the macOS archive and disk image...) it writes `<file>.minisig`, then
// `familiar-update.json` (version, release notes, each file's size and SHA-256) and its `.minisig`, and SHA256SUMS.txt.
// The signatures are minisign's (Ed25519 over the file's BLAKE2b-512, "ED"), so `minisign -Vm <file> -p <pubkey>` checks
// them too. Each trusted (signed) comment names the file and the version, and the app checks both.
//
// The secret key is minisign's unencrypted format (`minisign -G -W`); it is read from the environment (or
// --secret-file, for test fixtures), never printed, and must match --pubkey (the key compiled into the app), so a wrong
// or rotated secret fails here instead of shipping updates nobody can install.

import { createHash, createPrivateKey, createPublicKey, sign, verify } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const MANIFEST = "familiar-update.json";
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const VERSION = /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$/;

function args() {
  const out = {};
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i += 2) {
    if (!argv[i].startsWith("--") || argv[i + 1] === undefined) fail(`bad argument ${argv[i]}`);
    out[argv[i].slice(2)] = argv[i + 1];
  }
  return out;
}

function fail(message) {
  console.error(`sign-release: ${message}`);
  process.exit(1);
}

/** The base64 payload of a minisign key file (the line after the untrusted comment). */
function payload(text, what) {
  const lines = text.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  if (lines.length < 2 || !lines[0].startsWith("untrusted comment:")) fail(`${what} is not a minisign key file`);
  return Buffer.from(lines[1], "base64");
}

function secretKey(text) {
  const raw = payload(text, "the secret key");
  if (raw.length !== 158) fail("the secret key has the wrong length");
  if (raw.subarray(0, 2).toString() !== "Ed") fail("the secret key is not an Ed25519 minisign key");
  if (raw[2] !== 0 || raw[3] !== 0) fail("the secret key is password-protected; use an unencrypted key (minisign -G -W)");
  const keynum = raw.subarray(54, 62);
  const seed = raw.subarray(62, 94);
  const pk = raw.subarray(94, 126);
  const key = createPrivateKey({
    key: { kty: "OKP", crv: "Ed25519", d: seed.toString("base64url"), x: pk.toString("base64url") },
    format: "jwk",
  });
  // The embedded public half must be the seed's (a corrupted secret would sign with a key nobody trusts).
  const derived = Buffer.from(createPublicKey(key).export({ format: "jwk" }).x, "base64url");
  if (!derived.equals(pk)) fail("the secret key is corrupted");
  return { keynum, pk, key };
}

function publicKey(text) {
  const raw = payload(text, "the public key");
  if (raw.length !== 42 || raw.subarray(0, 2).toString() !== "Ed") fail("the public key is not an Ed25519 minisign key");
  return { keynum: raw.subarray(2, 10), pk: raw.subarray(10, 42) };
}

/** A minisign signature (prehashed "ED") of `data` with `trusted` as its signed comment. */
function signature(secret, data, trusted) {
  if (/[\r\n]/.test(trusted)) fail("a trusted comment can't span lines");
  const prehash = createHash("blake2b512").update(data).digest();
  const sig = sign(null, prehash, secret.key);
  const global = sign(null, Buffer.concat([sig, Buffer.from(trusted, "utf8")]), secret.key);
  const block = Buffer.concat([Buffer.from("ED"), secret.keynum, sig]);
  // Self-check with the public key the app trusts.
  if (!verify(null, prehash, createPublicKey(secret.key), sig)) fail("a fresh signature didn't verify");
  return `untrusted comment: signature from the Familiar release key\n${block.toString("base64")}\ntrusted comment: ${trusted}\n${global.toString("base64")}\n`;
}

const a = args();
const version = a.version ?? fail("--version is required");
if (!VERSION.test(version)) fail(`bad version '${version}'`);
const dir = a.dir ?? fail("--dir is required");
const trustedKey = publicKey(readFileSync(a.pubkey ?? fail("--pubkey is required"), "utf8"));
const secretText = a["secret-file"] ? readFileSync(a["secret-file"], "utf8") : process.env.FAMILIAR_UPDATE_KEY;
if (!secretText || !secretText.trim()) fail("FAMILIAR_UPDATE_KEY is not set: refusing to publish an unsigned release");
const secret = secretKey(secretText);
if (!secret.keynum.equals(trustedKey.keynum) || !secret.pk.equals(trustedKey.pk)) {
  fail("the secret key doesn't match the public key compiled into the app");
}
const notes = a.notes ? readFileSync(a.notes, "utf8").replace(/\r\n/g, "\n").trim() : "";
const timestamp = Math.floor(Date.now() / 1000);
// A pinned timestamp keeps test fixtures reproducible.
const stamp = a.timestamp ?? String(timestamp);

const artifacts = readdirSync(dir)
  .filter((name) => !name.endsWith(".minisig") && name !== MANIFEST && name !== "SHA256SUMS.txt")
  .filter((name) => statSync(join(dir, name)).isFile())
  .sort();
if (artifacts.length === 0) fail(`nothing to sign in ${dir}`);

const files = {};
for (const name of artifacts) {
  if (!SAFE_NAME.test(name)) fail(`unsafe artifact name '${name}'`);
  if (!name.includes(version)) fail(`'${name}' doesn't carry the version ${version}`);
  const data = readFileSync(join(dir, name));
  files[name] = { size: data.length, sha256: createHash("sha256").update(data).digest("hex") };
  writeFileSync(join(dir, `${name}.minisig`), signature(secret, data, `timestamp:${stamp}\tfile:${name}\tversion:${version}`));
  console.log(`signed ${name} (${data.length} bytes)`);
}

const manifest = Buffer.from(`${JSON.stringify({ schema: 1, version, notes, files }, null, 2)}\n`, "utf8");
writeFileSync(join(dir, MANIFEST), manifest);
writeFileSync(join(dir, `${MANIFEST}.minisig`), signature(secret, manifest, `timestamp:${stamp}\tfile:${MANIFEST}\tversion:${version}`));
console.log(`signed ${MANIFEST}`);

const sums = [...artifacts, MANIFEST]
  .map((name) => `${createHash("sha256").update(readFileSync(join(dir, name))).digest("hex")}  ${name}`)
  .join("\n");
writeFileSync(join(dir, "SHA256SUMS.txt"), `${sums}\n`);
