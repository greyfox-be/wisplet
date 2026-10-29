import { cpSync, mkdirSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';

const { version } = JSON.parse(readFileSync('package.json', 'utf8'));
const bundleDir = 'src-tauri/target/release/bundle';
const outDir = 'release';

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir);
for (const kind of ['nsis', 'msi']) {
  // Tauri never cleans bundle/, so older versions linger there.
  for (const file of readdirSync(join(bundleDir, kind)).filter((f) => f.includes(`_${version}_`))) {
    cpSync(join(bundleDir, kind, file), join(outDir, file));
    console.log(`${outDir}/${file}`);
  }
}
