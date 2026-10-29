import { readdir, stat } from "node:fs/promises";
import { join } from "node:path";
async function size(dir) {
  let total = 0;
  for (const item of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, item.name);
    total += item.isDirectory() ? await size(path) : (await stat(path)).size;
  }
  return total;
}
const bytes = await size("dist");
console.log(
  `Embedded UI: ${bytes.toLocaleString("en-US")} bytes (uncompressed)`,
);
if (bytes > 250 * 1024) throw new Error("Frontend exceeds the 250 KiB budget");
