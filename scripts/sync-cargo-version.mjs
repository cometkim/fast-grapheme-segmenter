#!/usr/bin/env node

// Mirrors the npm versions that `changeset version` just wrote onto the Cargo
// manifests of the crates published alongside them.
//
// Changesets has no notion of Cargo, but it does version private packages, so
// each publishable crate carries a private package.json holding the version
// changesets owns. This script copies that version into `[package] version` and
// refreshes Cargo.lock, keeping a release a single commit across both registries.
//
// Crates marked `publish = false` (rkyv-example, conformance) are skipped: they
// exist only inside the workspace and their versions are meaningless.

import assert from 'node:assert';
import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs/promises';
import { existsSync } from 'node:fs';
import * as path from 'node:path';

const rootDir = path.resolve(import.meta.dirname, '..');

const readJson = async file => JSON.parse(await fs.readFile(file, 'utf8'));

/**
 * Slice out a top-level TOML table, e.g. `[package]`, as `[start, end)` offsets
 * into `source`. Returns null when the table isn't present.
 */
function findTable(source, name) {
  const header = new RegExp(String.raw`^\[${name}\]\s*$`, 'm');
  const opening = header.exec(source);
  if (!opening) {
    return null;
  }
  const start = opening.index + opening[0].length;
  const next = /^\s*\[/m.exec(source.slice(start));
  return [start, next ? start + next.index : source.length];
}

/**
 * Rewrite the `version` key of a crate's `[package]` table. Also replaces
 * `version.workspace = true`, so a crate can be moved off workspace inheritance
 * without this script silently doing nothing.
 */
function setCrateVersion(manifest, version) {
  const table = findTable(manifest, 'package');
  assert(table, 'no [package] table');

  const [start, end] = table;
  const body = manifest.slice(start, end);
  const versionKey = /^version(\.workspace)?\s*=.*$/m;
  assert(versionKey.test(body), 'no version key in [package]');

  return (
    manifest.slice(0, start) +
    body.replace(versionKey, `version = "${version}"`) +
    manifest.slice(end)
  );
}

function crateIsPublishable(manifest) {
  const table = findTable(manifest, 'package');
  if (!table) {
    return false;
  }
  const body = manifest.slice(table[0], table[1]);
  return !/^publish\s*=\s*false\s*$/m.test(body);
}

function crateName(manifest) {
  const table = findTable(manifest, 'package');
  return table
    ? /^name\s*=\s*"([^"]+)"/m.exec(manifest.slice(table[0], table[1]))?.[1]
    : undefined;
}

async function workspaceDirs() {
  const { workspaces = [] } = await readJson(path.join(rootDir, 'package.json'));
  const dirs = new Set();
  for (const pattern of workspaces) {
    // Literal entries (the common case here) still round-trip through glob.
    for await (const match of fs.glob(pattern, { cwd: rootDir })) {
      dirs.add(match);
    }
  }
  return [...dirs].sort();
}

const updated = [];

for (const dir of await workspaceDirs()) {
  const manifestPath = path.join(rootDir, dir, 'Cargo.toml');
  if (!existsSync(manifestPath)) {
    continue;
  }

  const pkg = await readJson(path.join(rootDir, dir, 'package.json'));
  const manifest = await fs.readFile(manifestPath, 'utf8');

  if (!crateIsPublishable(manifest)) {
    continue;
  }

  // A publishable crate without a version to track is a setup error, not a
  // package to skip -- it would silently drift out of the release.
  assert(
    pkg.version,
    `${dir}/package.json has no "version" for its publishable crate to track`,
  );
  if (crateName(manifest) !== pkg.name) {
    console.warn(
      `warning: ${dir} publishes crate "${crateName(manifest)}" as npm package "${pkg.name}"`,
    );
  }

  const next = setCrateVersion(manifest, pkg.version);
  if (next !== manifest) {
    await fs.writeFile(manifestPath, next);
    updated.push(`${dir} -> ${pkg.version}`);
  }
}

if (updated.length === 0) {
  console.log('Cargo manifests already match their npm versions');
  process.exit(0);
}

console.log(`Synced Cargo versions:\n  ${updated.join('\n  ')}`);

// Cargo.lock pins workspace members by version, so it goes stale on every bump
// and `--locked` builds would fail on the release commit. `--workspace` keeps
// re-resolution to workspace members, leaving third-party pins alone; it still
// needs the registry index, so this can't run `--offline` on a cold runner.
const { status, error } = spawnSync('cargo', ['update', '--workspace'], {
  cwd: rootDir,
  stdio: 'inherit',
});
if (error?.code === 'ENOENT') {
  throw new Error('cargo is required to refresh Cargo.lock after a version bump');
}
process.exit(status ?? 1);
