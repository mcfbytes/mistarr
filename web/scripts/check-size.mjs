import { gzipSync } from 'node:zlib';
import { readFileSync } from 'node:fs';
import { readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

const BUDGET = 200 * 1024;
const dist = join(process.cwd(), 'dist');

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      out.push(...walk(full));
    } else {
      out.push(full);
    }
  }
  return out;
}

let total = 0;
for (const file of walk(dist)) {
  const buf = readFileSync(file);
  total += gzipSync(buf).length;
}

const kib = (total / 1024).toFixed(1);
console.log(`gzipped dist total: ${kib} KiB (budget 200 KiB)`);
if (total > BUDGET) {
  console.error('bundle exceeds budget');
  process.exit(1);
}
