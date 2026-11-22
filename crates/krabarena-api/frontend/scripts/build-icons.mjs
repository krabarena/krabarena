import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import svgstore from "svgstore";
import { optimize } from "svgo";

const root = resolve(import.meta.dirname, "..");
const lucideDir = resolve(root, "node_modules/lucide-static/icons");
const brandDir = resolve(root, "src/icons/brand");
const outputPath = resolve(root, "src/icons/krab-icons.svg");
const generatedIconDir = resolve(root, "src/icons/generated");

const ICONS = [
  ["bell", resolve(lucideDir, "bell.svg")],
  ["check", resolve(lucideDir, "check.svg")],
  ["copy", resolve(lucideDir, "copy.svg")],
  ["external-link", resolve(lucideDir, "external-link.svg")],
  ["globe", resolve(lucideDir, "globe.svg")],
  ["mail", resolve(lucideDir, "mail.svg")],
  ["message-circle", resolve(lucideDir, "message-circle.svg")],
  ["play", resolve(lucideDir, "play.svg")],
  ["search", resolve(lucideDir, "search.svg")],
  ["tag", resolve(lucideDir, "tag.svg")],
  ["user", resolve(lucideDir, "user.svg")],
  ["x", resolve(lucideDir, "x.svg")],
  ["github", resolve(brandDir, "github.svg")],
  ["google", resolve(brandDir, "google.svg")],
  ["discord", resolve(brandDir, "discord.svg")],
  ["linkedin", resolve(brandDir, "linkedin.svg")],
  ["x-logo", resolve(brandDir, "x-logo.svg")]
];

const LUCIDE_ICONS = new Set(ICONS.slice(0, 12).map(([id]) => id));
const MONOCHROME_BRAND_ICONS = new Set(["github", "discord", "linkedin", "x-logo"]);

const sprites = svgstore({
  cleanDefs: true,
  cleanSymbols: ["class", "style"],
  svgAttrs: {
    "aria-hidden": "true",
    class: "krab-icon-sprite"
  },
  symbolAttrs: {
    fill: null,
    stroke: null,
    style: null
  }
});

for (const [id, path] of ICONS) {
  const raw = await readFile(path, "utf8");
  const optimized = optimize(raw, {
    multipass: true,
    plugins: [
      "preset-default",
      {
        name: "removeAttrs",
        params: {
          attrs: "(class|style|data-name)"
        }
      }
    ]
  });
  const svg = iconSvg(id, optimized.data);
  sprites.add(`icon-${id}`, svg);
  await mkdir(generatedIconDir, { recursive: true });
  await writeFile(resolve(generatedIconDir, `${id}.svg`), `${svg}\n`);
}

await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, `${sprites.toString({ inline: true })}\n`);

function iconSvg(id, data) {
  if (MONOCHROME_BRAND_ICONS.has(id)) {
    return data.replace(/<path(?![^>]*\bfill=)/g, '<path fill="currentColor"');
  }
  if (!LUCIDE_ICONS.has(id)) return data;
  return data
    .replace(/<(path|circle|rect)(?![^>]*\bstroke=)/g, '<$1 stroke="currentColor"')
    .replace(/<(path|circle|rect)(?![^>]*\bfill=)/g, '<$1 fill="none"');
}
