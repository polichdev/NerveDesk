import { copyFile, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const uiRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const output = join(uiRoot, 'dist');
await mkdir(output, { recursive: true });
await Promise.all(['index.html', 'tray.html'].map(name =>
  copyFile(join(uiRoot, name), join(output, name))
));
