// Tiny trace helper. Gated by a flag the Rust side can flip at configure
// time so the spike can record which chrome.* paths Phantom hits without
// bloating the happy-path cost with console noise.
//
// Kept deliberately out of the public configure() surface: the Rust loader
// flips this via a placeholder string-replace before eval (`__TRACE_SHIM__`
// → "true"/"false"). Defaults to off so an unconfigured build is silent.

// The build substitutes this literal; `replace` is read as a compile-time
// constant after esbuild's define step if the Rust side ever opts into
// --define. For now we keep it as a runtime-mutable boolean set via
// configure().
let enabled = false;

/// Toggle trace mode. Called from `configure()` if the host passes
/// `{trace: true}`.
export function setTrace(on: boolean): void {
  enabled = on;
}

export function trace(area: string, msg: string, ...data: unknown[]): void {
  if (!enabled) return;
  // eslint-disable-next-line no-console
  console.log(`[chrome-shim:${area}]`, msg, ...data);
}

/// Unconditionally record that an unimplemented or stubbed `chrome.*` path was
/// hit, into a capped ring buffer on `globalThis.__extApiGaps`. This buffer is
/// the gap analysis: host tooling reads it back (e.g. the minimal host's
/// `minimal_host_introspect_bg`) to see exactly which `chrome.*` surfaces a
/// loaded extension actually exercised that the runtime does not fully serve.
/// Distinct from `__extLastErrors` (which captures thrown/rejected failures);
/// this captures *successful-but-fake* stub calls that would otherwise be
/// invisible. Deliberately independent of the `trace` flag.
export function recordGap(area: string, method: string): void {
  try {
    const g = globalThis as unknown as { __extApiGaps?: string[] };
    if (!g.__extApiGaps) g.__extApiGaps = [];
    const entry = `${area}.${method}`;
    const buf = g.__extApiGaps;
    // De-dupe consecutive repeats and cap at 100 to keep the buffer readable.
    if (buf[buf.length - 1] !== entry) buf.push(entry);
    if (buf.length > 100) buf.shift();
  } catch {
    /* recording must never throw into caller code */
  }
}

/// A trace helper specifically for stubbed methods. Always records the gap
/// (see [`recordGap`]); additionally console-warns when the trace flag is on.
export function traceStub(area: string, method: string, ...args: unknown[]): void {
  recordGap(area, method);
  if (!enabled) return;
  // eslint-disable-next-line no-console
  console.warn(`[chrome-shim:${area}] stub: ${method}`, ...args);
}
