// Bundles the whiteboard into data/whiteboard, which the app embeds and serves
// as agent-review://whiteboard/: the page, its script and styles, and
// Excalidraw's fonts, but for its Chinese, Japanese and Korean one (13 MB).

import { cp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import * as esbuild from "esbuild";

const out = "../data/whiteboard";
const excalidraw = "node_modules/@excalidraw/excalidraw";

await rm(out, { recursive: true, force: true });
await mkdir(out, { recursive: true });

// Turning Mermaid diagrams into drawings needs Mermaid, which is bigger than the rest together.
const noMermaid = {
  name: "no-mermaid",
  setup(build) {
    build.onResolve({ filter: /^@excalidraw\/mermaid-to-excalidraw$/ }, (args) => ({ path: args.path, namespace: "no-mermaid" }));
    build.onLoad({ filter: /.*/, namespace: "no-mermaid" }, () => ({
      contents: "export const parseMermaidToExcalidraw = () => Promise.reject(new Error('Mermaid is not available'));",
    }));
  },
};

const { metafile } = await esbuild.build({
  entryPoints: ["src/main.jsx"],
  outfile: `${out}/whiteboard.js`,
  bundle: true,
  minify: true,
  format: "iife",
  target: "safari16",
  jsx: "automatic",
  conditions: ["production"],
  define: { "process.env.NODE_ENV": '"production"', "process.env.IS_PREACT": '"false"' },
  legalComments: "eof",
  logLevel: "warning",
  // The styles find the fonts copied next to them.
  external: ["*.woff2"],
  plugins: [noMermaid],
  metafile: true,
});
await cp("src/index.html", `${out}/index.html`);
for (const font of await readdir(`${excalidraw}/dist/prod/fonts`)) {
  if (font !== "Xiaolai") {
    await cp(`${excalidraw}/dist/prod/fonts/${font}`, `${out}/fonts/${font}`, { recursive: true });
  }
}

// The licenses of the packages in the bundle. Those without a license file
// are MIT licensed: their notice is written out, with the holder's name.
const MIT = `Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`;
const HOLDERS = { "@excalidraw/excalidraw": "Excalidraw", "@radix-ui/": "WorkOS" };
const packages = new Set(
  Object.keys(metafile.inputs)
    .map((input) => input.match(/node_modules\/((?:@[^/]+\/)?[^/]+)\//)?.[1])
    .filter(Boolean),
);
const notices = [];
for (const name of [...packages].sort()) {
  const dir = `node_modules/${name}`;
  const manifest = JSON.parse(await readFile(`${dir}/package.json`, "utf8"));
  const file = (await readdir(dir)).find((f) => /^(licen[cs]e|copying)(\.|$|-)/i.test(f));
  let text;
  if (file) {
    text = await readFile(`${dir}/${file}`, "utf8");
  } else {
    const license = manifest.license ?? manifest.licenses?.[0]?.type;
    const holder =
      Object.entries(HOLDERS).find(([prefix]) => name.startsWith(prefix))?.[1] ??
      (typeof manifest.author === "string" ? manifest.author.replace(/\s*[<(].*$/, "") : manifest.author?.name);
    if (license !== "MIT" || !holder) {
      throw new Error(`No license notice for ${name}`);
    }
    text = `MIT License\n\nCopyright (c) ${holder}\n\n${MIT}`;
  }
  notices.push(`${name} ${manifest.version}\n\n${text.trim()}`);
}
await writeFile(`${out}/LICENSES.txt`, `${notices.join(`\n\n${"-".repeat(72)}\n\n`)}\n`);
