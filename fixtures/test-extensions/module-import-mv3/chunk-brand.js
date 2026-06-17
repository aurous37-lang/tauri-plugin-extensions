// Sibling chunk imported by sw.js via a relative specifier. Its export being
// observable in the SW proves the module graph (not just the entry) loaded
// from the extension root.
export const BRAND = "module-import-ok";

export function add(a, b) {
  return a + b;
}
