import { readdir, readFile } from "node:fs/promises";
import { relative, resolve } from "node:path";

const stylesDir = resolve(import.meta.dirname, "../src/styles");
const violations = [];

for (const path of await cssFiles(stylesDir)) {
  const file = relative(stylesDir, path);
  const source = await readFile(path, "utf8");
  if (/legacy/i.test(file)) {
    violations.push(`${file}: legacy CSS filenames are not allowed in src/styles`);
  }
  if (/u-tpl-[a-f0-9]+/i.test(source)) {
    violations.push(`${file}: temporary u-tpl-* classes are not allowed`);
  }
  if (/legacy-style\.css/i.test(source)) {
    violations.push(`${file}: legacy-style.css imports are not allowed`);
  }
  if (/\/static\/(?:style|theme-forum)\.css/i.test(source)) {
    violations.push(`${file}: direct legacy CSS file references are not allowed`);
  }
}

if (violations.length) {
  console.error(
    ["CSS architecture lint failed:", ...violations.map((violation) => `- ${violation}`)].join("\n")
  );
  process.exitCode = 1;
}

async function cssFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = await Promise.all(
    entries.map(async (entry) => {
      const path = resolve(dir, entry.name);
      if (entry.isDirectory()) return cssFiles(path);
      if (entry.isFile() && entry.name.endsWith(".css")) return [path];
      return [];
    })
  );
  return files.flat();
}
