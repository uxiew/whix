import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const directory = mkdtempSync(join(tmpdir(), 'whix-ui-migration-'));
const binary = fileURLToPath(new URL(`../../target/debug/whix${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const env = { ...process.env };
delete env.WHIX_PASSWORD;
delete env.WHIX_GUEST_PASSWORD;
const child = spawn(binary, ['--port', '25198', '--dir', directory, '--no-persist'], { env, stdio: 'inherit' });
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
child.on('error', (error) => { console.error(error.message); rmSync(directory, { recursive: true, force: true }); process.exit(1); });
child.on('exit', (code) => { rmSync(directory, { recursive: true, force: true }); process.exit(code ?? 0); });
