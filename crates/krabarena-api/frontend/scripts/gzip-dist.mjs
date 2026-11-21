import { gzip } from "node:zlib";
import { readdir, readFile, stat, writeFile } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { promisify } from "node:util";

const gzipAsync = promisify(gzip);
const distDir = resolve(import.meta.dirname, "../../static/dist");
const gzipExtensions = new Set([
  ".js",
  ".css",
  ".svg",
  ".json",
  ".woff",
  ".woff2",
  ".ttf",
  ".otf",
  ".eot",
  ".wasm"
]);
const criticalExtensions = new Set([".js", ".css"]);

async function walk(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await walk(path)));
    } else if (entry.isFile()) {
      files.push(path);
    }
  }
  return files;
}

const files = await walk(distDir);
for (const file of files) {
  if (file.endsWith(".gz")) {
    continue;
  }
  const ext = extname(file);
  if (!gzipExtensions.has(ext)) {
    continue;
  }

  try {
    const source = await readFile(file);
    const compressed = await gzipAsync(source, { level: 9 });
    await writeFile(`${file}.gz`, compressed);
  } catch (error) {
    if (criticalExtensions.has(ext)) {
      throw error;
    }
    console.warn(`Skipping gzip for ${file}: ${error.message}`);
  }
}

const criticalFiles = files.filter((file) => criticalExtensions.has(extname(file)));
for (const file of criticalFiles) {
  await stat(`${file}.gz`);
}
