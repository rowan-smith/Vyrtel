// Checks of how transport-soak.sh hands credentials to the server and connload: sign-in is always
// on, so without them connload's queries are refused and stored events cannot be reconciled.
//
//   node --test scripts/bench/*.test.mjs

import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';

const script = fs.readFileSync(path.join(import.meta.dirname, 'transport-soak.sh'), 'utf8');
// Each `docker run` command, with its continuation lines joined.
const runs = script
    .replace(/\\\r?\n/g, ' ')
    .split(/\r?\n/)
    .filter((l) => /^\s*docker run /.test(l));
const server = runs.find((r) => r.includes('/release/vyrtel'));
const load = runs.find((r) => r.includes('/release/connload'));

test('both containers get the admin password by name, never by value', () => {
    assert.ok(server && load, 'a server and a connload docker run');
    for (const run of [server, load]) {
        assert.match(run, /-e VYRTEL_AUTH_ADMIN_PASSWORD(\s|$)/);
        assert.doesNotMatch(run, /VYRTEL_AUTH_ADMIN_PASSWORD=/);
    }
    assert.match(script, /^export VYRTEL_AUTH_ADMIN_PASSWORD=\$\{ADMIN_PASSWORD:-/m);
});

test('connload creates an ingest key unless INGEST_AUTH=open', () => {
    assert.match(script, /key\) ingest_flag=--create-ingest-key ;;/);
    assert.match(script, /open\) ingest_flag= ;;/);
    assert.match(load, /\$ingest_flag "\$@"/);
});

test('the password is never echoed or written to an output file', () => {
    for (const line of script.split(/\r?\n/)) {
        if (/ADMIN_PASSWORD/.test(line) && !line.trimStart().startsWith('#')) {
            assert.doesNotMatch(line, /\b(echo|printf|tee)\b|>>?\s*"\$out/, line);
        }
    }
});
