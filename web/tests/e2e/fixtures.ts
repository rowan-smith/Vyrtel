import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));

const dir = path.resolve(here, '..', '..', '..', 'tests', 'fixtures');

/** Load a shared fixture, replacing `{{now-<n><unit>}}` with Unix nanoseconds. */
export function loadFixture(name: string): string {
  const text = fs.readFileSync(path.join(dir, name), 'utf8');
  const nowNs = BigInt(Date.now()) * 1_000_000n;
  return text.replace(/\{\{now([^}]*)\}\}/g, (_, expr: string) => {
    if (!expr) return nowNs.toString();
    const m = /^-(\d+)(ms|s|m)$/.exec(expr);
    if (!m) throw new Error(`bad placeholder ${expr}`);
    const mult = m[2] === 'ms' ? 1_000_000n : m[2] === 's' ? 1_000_000_000n : 60_000_000_000n;
    return (nowNs - BigInt(m[1]) * mult).toString();
  });
}
