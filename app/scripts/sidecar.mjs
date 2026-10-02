// Build the `dsnap` CLI in release mode and place it where Tauri's `externalBin` expects it
// (`src-tauri/binaries/dsnap-<target triple>[.exe]`), so the installer ships it next to the
// app (DSNA-75). Run by `beforeBuildCommand`.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const app = join(dirname(fileURLToPath(import.meta.url)), '..');
const root = join(app, '..');
const triple =
  process.env.TAURI_ENV_TARGET_TRIPLE ||
  execFileSync('rustc', ['--print', 'host-tuple'], { encoding: 'utf8' }).trim();
const exe = triple.includes('windows') ? '.exe' : '';

const args = ['build', '-p', 'dsnap-cli', '--release', '--locked'];
if (process.env.TAURI_ENV_TARGET_TRIPLE) args.push('--target', triple);
execFileSync('cargo', args, { cwd: root, stdio: 'inherit' });

const built = process.env.TAURI_ENV_TARGET_TRIPLE
  ? join(root, 'target', triple, 'release', `dsnap${exe}`)
  : join(root, 'target', 'release', `dsnap${exe}`);
const dest = join(app, 'src-tauri', 'binaries', `dsnap-${triple}${exe}`);
mkdirSync(dirname(dest), { recursive: true });
copyFileSync(built, dest);
console.log(`sidecar: ${dest}`);
