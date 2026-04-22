#!/usr/bin/env node
// tools/fig-converter/scripts/snapshot-specs.mjs
//
// Reads every specs/*.json file and writes a canonical-JSON rendering of it
// to specs/__snapshots__/<name>.snap.  Canonical form:
//
//   - object keys sorted recursively (lexicographic on code points)
//   - 2-space indent
//   - trailing newline
//
// Array order is preserved — in Fig specs, array order is semantic
// (e.g. the `subcommands` array).
//
// Idempotent: a second run over unchanged specs produces byte-identical
// output.  No dependencies beyond Node builtins.
//
// Usage:
//   node tools/fig-converter/scripts/snapshot-specs.mjs [--out <dir>]
//
// By default the output dir is <repo>/specs/__snapshots__.  The --out flag
// is used by scripts/check-snapshots.sh to snapshot into a scratch dir
// without mutating the working tree.

import { readFileSync, writeFileSync, readdirSync, mkdirSync, existsSync } from 'node:fs';
import { join, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
// scripts/snapshot-specs.mjs → tools/fig-converter/scripts → tools/fig-converter → tools → <repo>
const REPO_ROOT = join(__dirname, '..', '..', '..');
const SPECS_DIR = join(REPO_ROOT, 'specs');

// ---- arg parsing -----------------------------------------------------------

function parseArgs(argv) {
    const args = { out: null };
    for (let i = 0; i < argv.length; i++) {
        const a = argv[i];
        if (a === '--out') {
            args.out = argv[++i];
            if (!args.out) {
                console.error('error: --out requires a directory argument');
                process.exit(2);
            }
        } else if (a === '--help' || a === '-h') {
            printHelp();
            process.exit(0);
        } else {
            console.error(`error: unknown argument: ${a}`);
            process.exit(2);
        }
    }
    return args;
}

function printHelp() {
    process.stdout.write(
        'Usage: node snapshot-specs.mjs [--out <dir>]\n' +
            '\n' +
            'Reads every specs/*.json and writes a canonical-JSON snapshot to\n' +
            '<out>/<name>.snap.  Default <out> is <repo>/specs/__snapshots__.\n',
    );
}

// ---- canonical JSON --------------------------------------------------------

/**
 * Recursively return a structural copy of `value` with object keys sorted
 * (lexicographic).  Arrays keep their original order.  Primitives are
 * returned as-is.
 */
function canonicalize(value) {
    if (Array.isArray(value)) {
        return value.map(canonicalize);
    }
    if (value !== null && typeof value === 'object') {
        const sortedKeys = Object.keys(value).sort();
        const out = {};
        for (const k of sortedKeys) {
            out[k] = canonicalize(value[k]);
        }
        return out;
    }
    return value;
}

function renderCanonical(value) {
    return JSON.stringify(canonicalize(value), null, 2) + '\n';
}

// ---- main ------------------------------------------------------------------

function main() {
    const args = parseArgs(process.argv.slice(2));
    const outDir = args.out ?? join(SPECS_DIR, '__snapshots__');

    if (!existsSync(SPECS_DIR)) {
        console.error(`error: specs dir not found: ${SPECS_DIR}`);
        process.exit(2);
    }

    mkdirSync(outDir, { recursive: true });

    const entries = readdirSync(SPECS_DIR, { withFileTypes: true })
        .filter((d) => d.isFile() && d.name.endsWith('.json'))
        .map((d) => d.name)
        .sort();

    let written = 0;
    for (const file of entries) {
        const name = basename(file, '.json');
        const src = join(SPECS_DIR, file);
        const dst = join(outDir, `${name}.snap`);

        let parsed;
        try {
            const raw = readFileSync(src, 'utf8');
            parsed = JSON.parse(raw);
        } catch (err) {
            console.error(`error: failed to parse ${src}: ${err.message}`);
            process.exit(1);
        }

        const rendered = renderCanonical(parsed);
        writeFileSync(dst, rendered, 'utf8');
        written++;
    }

    console.log(`snapshot-specs: wrote ${written} snapshot(s) to ${outDir}`);
}

main();
