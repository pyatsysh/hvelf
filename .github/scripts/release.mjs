import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { appendFileSync, copyFileSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const platforms = {
  'linux-x86_64': ['deb', '.deb'],
  'windows-x86_64': ['nsis', '.exe'],
  'macos-x86_64': ['dmg', '.dmg'],
  'macos-arm64': ['dmg', '.dmg'],
};
const json = path => JSON.parse(readFileSync(path, 'utf8'));
const digest = path => createHash('sha256').update(readFileSync(path)).digest('hex');

export function check(root, ref = '') {
  const pkg = json(join(root, 'package.json'));
  const config = json(join(root, 'src-tauri/tauri.conf.json'));
  const cargo = readFileSync(join(root, 'src-tauri/Cargo.toml'), 'utf8');
  const section = cargo.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  const version = section?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  assert.match(pkg.name, /^[a-z0-9-]+$/, 'package name must be safe in artifact names');
  assert.match(pkg.version, /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/, 'invalid version');
  assert.equal(version, pkg.version, 'Cargo and npm versions differ');
  assert.equal(config.version, pkg.version, 'Tauri and npm versions differ');
  const tag = `v${pkg.version}`;
  if (ref.startsWith('refs/tags/')) assert.equal(ref, `refs/tags/${tag}`, 'tag must match the application version');
  return { name: pkg.name, version: pkg.version, tag };
}

export function collect(root, platform, bundle, env = process.env) {
  const app = check(root, env.GITHUB_REF);
  assert.ok(Object.hasOwn(platforms, platform), 'unknown platform');
  const [expectedBundle, extension] = platforms[platform];
  assert.equal(bundle, expectedBundle, 'wrong bundle for platform');
  const target = env.CARGO_TARGET_DIR || join(root, 'src-tauri/target');
  const dir = join(target, 'release/bundle', bundle);
  const candidates = readdirSync(dir).filter(name => name.endsWith(extension));
  assert.equal(candidates.length, 1, 'expected exactly one installer in a clean build');
  const output = join(root, 'dist');
  mkdirSync(output, { recursive: true });
  const filename = `${app.name}_${app.version}_${platform}${extension}`;
  const destination = join(output, filename);
  copyFileSync(join(dir, candidates[0]), destination);
  const receipt = { ...app, platform, bundle, filename, sha256: digest(destination),
    commit: env.GITHUB_SHA || null, workflowRun: env.GITHUB_RUN_ID || null,
    runnerOS: env.RUNNER_OS || null, runnerArch: env.RUNNER_ARCH || null,
    qualification: 'Build and automated tests only. Native desktop acceptance remains separate.' };
  writeFileSync(join(output, `${filename}.json`), JSON.stringify(receipt, null, 2) + '\n');
  return receipt;
}

export function finalise(root, env = process.env) {
  const app = check(root, env.GITHUB_REF);
  assert.match(env.GITHUB_SHA || '', /^[a-f0-9]{40}$/, 'release requires the exact source commit');
  const dir = join(root, 'dist');
  const names = readdirSync(dir).sort();
  const receipts = names.filter(name => name.endsWith('.json')).map(name => json(join(dir, name)));
  assert.deepEqual(receipts.map(r => r.platform).sort(), Object.keys(platforms).sort(), 'all four platforms must be present exactly once');
  const expected = [];
  for (const receipt of receipts) {
    assert.equal(receipt.name, app.name);
    assert.equal(receipt.version, app.version);
    assert.equal(receipt.commit, env.GITHUB_SHA, 'packages must come from this commit');
    const [bundle, ext] = platforms[receipt.platform];
    assert.equal(receipt.bundle, bundle);
    const filename = `${app.name}_${app.version}_${receipt.platform}${ext}`;
    assert.equal(receipt.filename, filename);
    assert.equal(basename(filename), filename);
    assert.equal(digest(join(dir, filename)), receipt.sha256, 'installer checksum mismatch');
    expected.push(filename, `${filename}.json`);
  }
  assert.deepEqual(names.filter(n => n !== 'SHA256SUMS'), expected.sort(), 'unexpected release files');
  const sums = expected.map(name => `${digest(join(dir, name))}  ${name}\n`).join('');
  writeFileSync(join(dir, 'SHA256SUMS'), sums);
  return sums;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const root = process.cwd();
    const [command, platform, bundle] = process.argv.slice(2);
    if (command === 'check') {
      const app = check(root, process.env.GITHUB_REF);
      if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `tag=${app.tag}\n`);
      console.log(JSON.stringify(app));
    } else if (command === 'collect') console.log(JSON.stringify(collect(root, platform, bundle)));
    else if (command === 'finalise') console.log(finalise(root));
    else throw new Error('usage: release.mjs check | collect PLATFORM BUNDLE | finalise');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
