#!/usr/bin/env node
// scripts/check-corpus-hash.mjs
//
// Determinism gate for the fig-converter emit path. Runs the converter
// twice into separate temp dirs and asserts that the resulting
// `corpus-hash.txt` is byte-identical between runs.
//
// On hash mismatch the script exits 1 so CI can fail the job. On success
// it prints the shared digest to stdout (so logs make it easy to grep
// the active corpus hash from a build).
//
// Usage:
//   node tools/fig-converter/scripts/check-corpus-hash.mjs [--specs name1,name2]
//
// `--specs` is forwarded to the converter so callers can run on a small
// subset (CI uses this for PRs to keep wall-clock low while still
// exercising the determinism contract — see ci.yml).

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const CONVERTER = join(__dirname, '..', 'src', 'index.js');

const { values } = parseArgs({
    options: {
        specs: { type: 'string' },
        'batch-size': { type: 'string' },
    },
});

const requestedSpecs =
    values.specs === undefined
        ? null
        : [...new Set(values.specs.split(',').map((s) => s.trim()).filter(Boolean))].sort();

function countJsonSpecs(dir) {
    let count = 0;
    const queue = [dir];
    while (queue.length > 0) {
        const current = queue.shift();
        for (const entry of readdirSync(current, { withFileTypes: true })) {
            const path = join(current, entry.name);
            if (entry.isDirectory()) {
                queue.push(path);
            } else if (entry.isFile() && entry.name.endsWith('.json')) {
                count++;
            }
        }
    }
    return count;
}

function assertConvertedSpecCount(dir, label) {
    const actual = countJsonSpecs(dir);
    if (requestedSpecs === null) {
        if (actual === 0) {
            throw new Error(`[corpus-hash] ${label}: converter emitted no JSON specs`);
        }
        return;
    }

    if (requestedSpecs.length === 0) {
        throw new Error(`[corpus-hash] ${label}: --specs requested no specs`);
    }
    if (actual !== requestedSpecs.length) {
        throw new Error(
            `[corpus-hash] ${label}: converter emitted ${actual} JSON spec(s), expected ${requestedSpecs.length}`,
        );
    }
}

function runConverter(outDir) {
    const args = [
        CONVERTER,
        `--output=${outDir}`,
        '--deterministic',
    ];
    if (values.specs !== undefined) {
        args.push(`--specs=${values.specs}`);
    }
    if (values['batch-size']) {
        args.push(`--batch-size=${values['batch-size']}`);
    }
    execFileSync(process.execPath, args, {
        stdio: 'inherit',
    });
}

function main() {
    // Both tempdirs are created INSIDE the try so that a throw from the
    // second `mkdtempSync` (or any prior step) doesn't leak the first.
    // The finally guards each rmSync with a null check.
    let dir1 = null;
    let dir2 = null;
    try {
        dir1 = mkdtempSync(join(tmpdir(), 'gc-corpus-'));
        dir2 = mkdtempSync(join(tmpdir(), 'gc-corpus-'));

        process.stderr.write(`[corpus-hash] run 1 -> ${dir1}\n`);
        runConverter(dir1);
        assertConvertedSpecCount(dir1, 'run 1');
        process.stderr.write(`[corpus-hash] run 2 -> ${dir2}\n`);
        runConverter(dir2);
        assertConvertedSpecCount(dir2, 'run 2');

        const h1 = readFileSync(join(dir1, 'corpus-hash.txt'), 'utf8').trim();
        const h2 = readFileSync(join(dir2, 'corpus-hash.txt'), 'utf8').trim();

        if (h1 !== h2) {
            process.stderr.write(`Hash mismatch:\n  run 1: ${h1}\n  run 2: ${h2}\n`);
            // Set exitCode (instead of process.exit) so the finally below
            // still runs and cleans up both tempdirs.
            process.exitCode = 1;
            return;
        }
        process.stdout.write(`OK ${h1}\n`);
    } finally {
        if (dir1) rmSync(dir1, { recursive: true, force: true });
        if (dir2) rmSync(dir2, { recursive: true, force: true });
    }
}

main();
