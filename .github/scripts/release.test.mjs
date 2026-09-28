import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync, unlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { check, collect, finalise, platforms } from './release.mjs';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'release-fixture-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri'));
  writeFileSync(join(root, 'package.json'), JSON.stringify({ name: 'example', version: '0.1.0' }));
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ version: '0.1.0' }));
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nname = "example"\nversion = "0.1.0"\n\n[dependencies]\n');
  return root;
}
const env = { GITHUB_REF: 'refs/tags/v0.1.0', GITHUB_SHA: 'a'.repeat(40) };
function packageAll(root) {
  for (const [platform, [bundle, extension]] of Object.entries(platforms)) {
    const dir = join(root, 'src-tauri/target/release/bundle', bundle);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, `example${extension}`), `fixture for ${platform}`);
    collect(root, platform, bundle, env);
  }
}

test('version tags must match all application metadata', t => {
  const root = fixture(t);
  assert.equal(check(root, env.GITHUB_REF).tag, 'v0.1.0');
  assert.throws(() => check(root, 'refs/tags/v9.0.0'), /tag must match/);
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), '{"version":"0.2.0"}');
  assert.throws(() => check(root), /Tauri and npm/);
});

test('Cargo version cannot drift from the installer version', t => {
  const root = fixture(t);
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nversion = "0.2.0"\n');
  assert.throws(() => check(root), /Cargo and npm/);
});

test('collection refuses missing, ambiguous and wrong-platform installers', t => {
  const root = fixture(t);
  assert.throws(() => collect(root, 'linux-x86_64', 'nsis', env), /wrong bundle/);
  assert.throws(() => collect(root, 'linux-x86_64', 'deb', env), /ENOENT/);
  const dir = join(root, 'src-tauri/target/release/bundle/deb');
  mkdirSync(dir, { recursive: true });
  for (const name of ['old.deb', 'new.deb']) writeFileSync(join(dir, name), 'fixture');
  assert.throws(() => collect(root, 'linux-x86_64', 'deb', env), /exactly one/);
});

test('complete release has checksums and retains the exact source commit', t => {
  const root = fixture(t);
  packageAll(root);
  const sums = finalise(root, env);
  assert.equal(sums.trim().split('\n').length, 8);
  assert.equal(sums, readFileSync(join(root, 'dist/SHA256SUMS'), 'utf8'));
  assert.throws(() => finalise(root, { ...env, GITHUB_SHA: 'b'.repeat(40) }), /this commit/);
});

test('missing platforms and tampered packages cannot become a release', t => {
  const root = fixture(t);
  packageAll(root);
  const file = join(root, 'dist/example_0.1.0_linux-x86_64.deb');
  writeFileSync(file, 'tampered');
  assert.throws(() => finalise(root, env), /checksum mismatch/);
  unlinkSync(`${file}.json`);
  assert.throws(() => finalise(root, env), /all four platforms/);
});
