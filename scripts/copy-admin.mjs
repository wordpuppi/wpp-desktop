// Copies the built @wordpuppi/admin bundle into the shell dist root so the
// shell's iframe can load it at `/index.html`. Tolerant of a missing admin
// build so the `tauri build --debug` smoke still produces a .app before
// Task B has built wpp-admin. Run AFTER `build:shell` (which empties dist).
import { cp, access, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const adminDist = resolve(here, '../../wpp-admin/dist');
const shellDist = resolve(here, '../dist');

await mkdir(shellDist, { recursive: true });

try {
  await access(adminDist);
} catch {
  console.warn(
    '[copy-admin] wpp-admin/dist not found — the iframe will 404 until it is built.\n' +
      '            Run `pnpm --filter @wordpuppi/admin build`, then rebuild the desktop app.',
  );
  process.exit(0);
}

// Merge admin dist into shell dist. shell.html is untouched; admin's
// index.html + assets land at the root. Hashed asset names avoid collisions.
await cp(adminDist, shellDist, { recursive: true, force: true });
console.log('[copy-admin] copied wpp-admin/dist → dist');
