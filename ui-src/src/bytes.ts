export const MAX_BODY_FILE = 8 * 1024 * 1024;

export function base64Of(bytes: Uint8Array): string {
  const chunks: string[] = [];
  for (let at = 0; at < bytes.length; at += 16384) {
    chunks.push(String.fromCharCode(...bytes.subarray(at, at + 16384)));
  }
  return btoa(chunks.join(''));
}

export function bytesOf(base64: string): Uint8Array {
  const padding = base64.endsWith('==') ? 2 : base64.endsWith('=') ? 1 : 0;
  const data = padding ? base64.slice(0, -padding) : base64;
  if (base64.length % 4 || /[^A-Za-z0-9+/]/.test(data)) throw new Error('Invalid base64');
  return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
}

export async function bodyFile(file: File): Promise<string> {
  if (file.size > MAX_BODY_FILE) throw new Error('Body file exceeds 8 MiB');
  return base64Of(new Uint8Array(await file.arrayBuffer()));
}
