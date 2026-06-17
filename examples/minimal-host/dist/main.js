// Plain ES module — no bundler. Uses window.__TAURI_INTERNALS__.invoke so we
// don't need to ship @tauri-apps/api through a build step.
//
// Tauri v2 exposes `__TAURI_INTERNALS__.invoke(cmd, args)` to every window
// regardless of whether withGlobalTauri is set. See
// https://v2.tauri.app/reference/javascript/api/namespacecore/#invoke

/**
 * @typedef {{id: {0: string}, summary: null | {id: {0: string}, name: string, version: string, description: string|null}}} LoadResult
 */

const $ = (id) => /** @type {HTMLElement} */ (document.getElementById(id));

/** @type {(cmd: string, args?: Record<string, unknown>) => Promise<any>} */
function invoke(cmd, args) {
  const internals = /** @type {any} */ (window).__TAURI_INTERNALS__;
  if (!internals || typeof internals.invoke !== "function") {
    return Promise.reject(
      new Error(
        "Tauri IPC not available — this page must be served inside the Tauri webview, not a browser.",
      ),
    );
  }
  return internals.invoke(cmd, args ?? {});
}

const logEl = $("log");
const extDumpEl = /** @type {HTMLPreElement} */ ($("extensions-dump"));
const fixturePathEl = /** @type {HTMLInputElement} */ ($("fixture-path"));
const loadedIdEl = /** @type {HTMLInputElement} */ ($("loaded-id"));
const pingBgBtn = /** @type {HTMLButtonElement} */ ($("ping-background"));
const phantomFixtureEl = /** @type {HTMLInputElement} */ ($("phantom-fixture-path"));
const phantomLoadedIdEl = /** @type {HTMLInputElement} */ ($("phantom-loaded-id"));
const testDappUrlEl = /** @type {HTMLInputElement} */ ($("test-dapp-url"));
const phantomProbeEl = /** @type {HTMLPreElement} */ ($("phantom-probe"));
const phantomLifecycleEl = /** @type {HTMLPreElement} */ ($("phantom-lifecycle"));

let lastLoadedId = null;

function log(message, level = "info") {
  const ts = new Date().toISOString().slice(11, 19);
  const line = document.createElement("div");
  line.className = `log-line ${level === "info" ? "" : level}`.trim();
  line.textContent = `[${ts}] ${message}`;
  logEl.appendChild(line);
  logEl.scrollTop = logEl.scrollHeight;
}

function asJson(value) {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

async function refreshFixturePath() {
  try {
    const p = await invoke("minimal_host_noop_fixture_path");
    fixturePathEl.value = p;
    log(`fixture path: ${p}`, "ok");
  } catch (e) {
    log(`fixture path lookup failed: ${e instanceof Error ? e.message : e}`, "err");
  }
}

async function pingIpc() {
  try {
    const r = await invoke("minimal_host_ping");
    log(`ping -> ${r}`, "ok");
  } catch (e) {
    log(`ping failed: ${e instanceof Error ? e.message : e}`, "err");
  }
}

async function loadNoop() {
  const p = fixturePathEl.value?.trim();
  if (!p) {
    log("no fixture path — click refresh first", "warn");
    return;
  }
  log(`loading unpacked: ${p}`);
  try {
    /** @type {LoadResult} */
    const r = await invoke("minimal_host_load_unpacked", { path: p });
    // ExtensionId is `struct ExtensionId(String)` — serde emits it as the
    // tuple-struct with a nested .0 on the JS side. Handle either shape.
    const idStr = typeof r.id === "string" ? r.id : r.id?.[0] ?? r.id?.value ?? asJson(r.id);
    lastLoadedId = idStr;
    loadedIdEl.value = idStr;
    pingBgBtn.disabled = false;
    log(`loaded ok -> id=${idStr}`, "ok");
    log(`summary -> ${asJson(r.summary)}`);
    await listExtensions();
  } catch (e) {
    log(`load failed: ${e instanceof Error ? e.message : e}`, "err");
  }
}

async function listExtensions() {
  try {
    const r = await invoke("minimal_host_list_extensions");
    extDumpEl.textContent = asJson(r);
    log(`list -> ${Array.isArray(r) ? r.length : "?"} extension(s)`, "ok");
  } catch (e) {
    log(`list failed: ${e instanceof Error ? e.message : e}`, "err");
  }
}

async function pingBackground() {
  if (!lastLoadedId) {
    log("load noop-mv3 first", "warn");
    return;
  }
  log(`pinging background for ${lastLoadedId}`);
  try {
    const r = await invoke("minimal_host_ping_background", {
      extensionId: lastLoadedId,
    });
    log(`background -> ${r}`, "ok");
  } catch (e) {
    log(`background ping failed: ${e instanceof Error ? e.message : e}`, "warn");
  }
}

function clearLog() {
  logEl.textContent = "";
}

$("refresh-fixture-path").addEventListener("click", refreshFixturePath);
$("ping-ipc").addEventListener("click", pingIpc);
$("load-noop").addEventListener("click", loadNoop);
$("list-extensions").addEventListener("click", listExtensions);
$("ping-background").addEventListener("click", pingBackground);
$("clear-log").addEventListener("click", clearLog);

// --- Auto-acceptance ---
// Runs the full click-sequence programmatically on page load, records every
// result, and writes the report to disk via minimal_host_write_acceptance_report.
// The developer reading this session's trace picks up the file; the user
// does not need to click anything.

async function step(name, fn) {
  const t0 = performance.now();
  try {
    const result = await fn();
    const ms = Math.round(performance.now() - t0);
    log(`[${name}] ok (${ms}ms)`, "ok");
    return { name, ok: true, ms, result };
  } catch (e) {
    const ms = Math.round(performance.now() - t0);
    const message = e instanceof Error ? e.message : String(e);
    log(`[${name}] err: ${message}`, "err");
    return { name, ok: false, ms, error: message };
  }
}

async function runAcceptance() {
  log("─ acceptance run begin ─");
  const report = {
    startedAt: new Date().toISOString(),
    steps: /** @type {any[]} */ ([]),
  };

  report.steps.push(await step("fixture_path", () => invoke("minimal_host_noop_fixture_path")));
  report.steps.push(await step("ping_ipc", () => invoke("minimal_host_ping")));

  const fixturePath = report.steps[0].ok ? report.steps[0].result : null;
  if (fixturePath) fixturePathEl.value = fixturePath;

  const listBefore = await step("list_before_load", () => invoke("minimal_host_list_extensions"));
  report.steps.push(listBefore);

  const load = await step("load_unpacked", () => invoke("minimal_host_load_unpacked", { path: fixturePath }));
  report.steps.push(load);
  const loadedId = load.ok
    ? typeof load.result.id === "string"
      ? load.result.id
      : load.result.id?.[0] ?? null
    : null;
  if (loadedId) {
    lastLoadedId = loadedId;
    loadedIdEl.value = loadedId;
    pingBgBtn.disabled = false;
  }

  const listAfter = await step("list_after_load", () => invoke("minimal_host_list_extensions"));
  report.steps.push(listAfter);
  if (listAfter.ok) extDumpEl.textContent = asJson(listAfter.result);

  report.steps.push(
    await step("list_webview_windows", () => invoke("minimal_host_list_webview_windows")),
  );

  if (loadedId) {
    report.steps.push(
      await step("ping_background", () =>
        invoke("minimal_host_ping_background", { extensionId: loadedId }),
      ),
    );
  } else {
    report.steps.push({ name: "ping_background", ok: false, skipped: true, reason: "no loadedId" });
  }

  // Reload regression — the reload-after-install race. A second
  // load_unpacked against the same path triggers stop (close BG webview)
  // → respawn under the same ext-bg-<id> label. Before the lifecycle fix
  // the respawn hit AlreadyExists (wry's close() is async-dispatched) and
  // the entry ended up Running { bg_handle: None }; this phase fails red
  // in that state and is the live half of tests/bg_reload_race.rs.
  if (loadedId && fixturePath) {
    report.steps.push(
      await step("reload_unpacked", () =>
        invoke("minimal_host_load_unpacked", { path: fixturePath }),
      ),
    );
    // Let the old window's close fully settle before observing topology —
    // otherwise a lingering not-yet-closed window masks the missing respawn.
    await new Promise((r) => setTimeout(r, 1500));
    report.steps.push(
      await step("ping_background_after_reload", async () => {
        const raw = await invoke("minimal_host_ping_background", { extensionId: loadedId });
        const parsed = JSON.parse(raw);
        if (!parsed.bgWindowPresent) {
          throw new Error(`BG webview missing after reload: ${raw}`);
        }
        return parsed;
      }),
    );
    // The load-bearing routing assertion: a real chrome.runtime message
    // round-trip into the noop fixture's BG worker. Proves the dispatch
    // polyfill + configure + shim onMessage + sendResponse + response-phase
    // invoke chain end-to-end (tests/runtime_routing.rs pins the pure
    // pieces; this is the live half).
    report.steps.push(
      await step("bg_round_trip", async () => {
        const raw = await invoke("minimal_host_ping_background", { extensionId: loadedId });
        const parsed = JSON.parse(raw);
        const rt = parsed.roundTrip;
        if (!rt?.ok) {
          throw new Error(`background round-trip failed: ${asJson(rt)}`);
        }
        if (rt.response?.kind !== "pong") {
          throw new Error(`expected a pong from the noop BG, got: ${asJson(rt.response)}`);
        }
        return rt;
      }),
    );
    // Diagnostic (never gates the run): snapshot the BG webview's shim
    // wiring + buffered errors through the probe slot.
    report.steps.push(
      await step("bg_introspect", async () => {
        await invoke("minimal_host_introspect_bg", { extensionId: loadedId });
        await new Promise((r) => setTimeout(r, 750));
        return invoke("minimal_host_probe_dapp");
      }),
    );
    report.steps.push(
      await step("diagnostics_after_reload", async () => {
        const d = await invoke("plugin:extensions|extensions_diagnostics");
        if (!d.invariants_ok) {
          throw new Error(`invariant violations: ${asJson(d.invariant_violations)}`);
        }
        if (d.count_bg_windows_orphaned !== 0) {
          throw new Error(`orphaned BG windows: ${d.count_bg_windows_orphaned}`);
        }
        return d;
      }),
    );
  }

  // Phase 1.5 — BG-host fixtures (D-008): module SW + importScripts + module
  // SW that registers a MAIN-world content script. These GATE the report:
  // they are deterministic fixtures we control, so a failure means the
  // faithful background host regressed.
  await runBgHostFixtures(report);

  // Phase 2 — Phantom (D-005 v1 acceptance target).
  await runPhantomPhase(report);

  // Phase 3 — EVM provider matrix (MetaMask / Rabby / Phantom EVM).
  await runEvmMatrix(report);

  report.finishedAt = new Date().toISOString();
  report.ok = report.steps.every((s) => s.ok || s.skipped);

  // Persist — the developer reads examples/minimal-host/acceptance-report.json.
  const payload = asJson(report);
  try {
    const path = await invoke("minimal_host_write_acceptance_report", { reportJson: payload });
    log(`acceptance report -> ${path}`, "ok");
  } catch (e) {
    log(`acceptance report write failed: ${e instanceof Error ? e.message : e}`, "err");
  }

  log(`─ acceptance run ${report.ok ? "OK" : "FAILED"} ─`, report.ok ? "ok" : "err");
}

// --- Phantom phase ---
// Locates the vendored Phantom extension, loads it via the plugin, opens the
// canary test-dapp in a second webview window, then reads back the dapp's
// probe via the Rust-side ProbeSlot. Lifecycle events fired during the load
// are captured by subscribing to extensions://lifecycle/changed.

/** Subscribe to a Tauri v2 event without pulling in @tauri-apps/api.
 *
 * Tauri v2's event bus is exposed to every webview via the core `event`
 * plugin. Commands:
 *   - plugin:event|listen  — args {event: string, target?: {kind: "Any"|"AnyLabel"|...}, handler: number}
 *   - plugin:event|unlisten — args {event: string, eventId: number}
 *
 * The plugin dispatches events by calling `window[`_${handler}`](payload)`
 * on the webview. We allocate a handler fn with a stable name, register it
 * on window, and tear it all down in the returned unlisten closure.
 * Returns null if the event plugin can't be reached (shouldn't happen on
 * Tauri v2 but handled defensively).
 */
async function subscribeLifecycle(sink) {
  // Allocate a unique handler slot. Tauri expects to call
  // window[`_${id}`](serializedJson). We use Math.random for the slot name;
  // collisions with user code are implausible for a single listener.
  const handlerId = Math.floor(Math.random() * 2 ** 31);
  const slot = `_${handlerId}`;
  /** @type {any} */ (window)[slot] = (rawJson) => {
    try {
      const parsed = typeof rawJson === "string" ? JSON.parse(rawJson) : rawJson;
      // Tauri's event callback shape: {event, id, payload}. Store payload if
      // present, else the whole envelope.
      sink.push(parsed && "payload" in parsed ? parsed.payload : parsed);
    } catch {
      sink.push(rawJson);
    }
  };

  try {
    const eventId = await invoke("plugin:event|listen", {
      event: "extensions://lifecycle/changed",
      target: { kind: "Any" },
      handler: handlerId,
    });
    return async () => {
      try {
        await invoke("plugin:event|unlisten", {
          event: "extensions://lifecycle/changed",
          eventId,
        });
      } catch {
        /* best-effort */
      }
      delete (/** @type {any} */ (window))[slot];
    };
  } catch (e) {
    delete (/** @type {any} */ (window))[slot];
    throw e;
  }
}

async function runPhantomPhase(report) {
  log("─ phantom phase begin ─");
  const phantomSteps = /** @type {any[]} */ ([]);

  const fixtureStep = await step("phantom_fixture_path", () =>
    invoke("minimal_host_phantom_fixture_path"),
  );
  phantomSteps.push(fixtureStep);
  if (!fixtureStep.ok) {
    log("phantom fixture unavailable — skipping phantom phase", "warn");
    report.phantom = { skipped: true, reason: fixtureStep.error, steps: phantomSteps };
    report.steps.push(...phantomSteps);
    return;
  }
  phantomFixtureEl.value = fixtureStep.result;

  // Subscribe BEFORE the load so we capture Installed + Started events.
  const lifecycleEvents = /** @type {any[]} */ ([]);
  let unlisten = null;
  try {
    unlisten = await subscribeLifecycle(lifecycleEvents);
    if (unlisten) log("subscribed to extensions://lifecycle/changed", "ok");
    else log("lifecycle subscription unavailable in this build", "warn");
  } catch (e) {
    log(`lifecycle subscribe failed: ${e instanceof Error ? e.message : e}`, "warn");
  }

  const loadStep = await step("phantom_load", () =>
    invoke("minimal_host_load_unpacked", { path: fixtureStep.result }),
  );
  phantomSteps.push(loadStep);
  const phantomId = loadStep.ok
    ? typeof loadStep.result.id === "string"
      ? loadStep.result.id
      : loadStep.result.id?.[0] ?? null
    : null;
  if (phantomId) phantomLoadedIdEl.value = phantomId;

  // Give Phantom's BG service worker a moment to spin up under WebView2
  // before we observe window topology. Empirically 2s is comfortable for a
  // cold load; first boot of any BG can take the MSIX/MSEdge runtime a tick.
  await new Promise((r) => setTimeout(r, 2000));

  phantomSteps.push(
    await step("list_windows_after_phantom_load", () =>
      invoke("minimal_host_list_webview_windows"),
    ),
  );

  if (phantomId) {
    phantomSteps.push(
      await step("phantom_ping_background", () =>
        invoke("minimal_host_ping_background", { extensionId: phantomId }),
      ),
    );
  }

  // Resolve the test-dapp URL, open a second webview window at it, then
  // give the dapp's 5s probe-poll time to complete + POST its result.
  const urlStep = await step("test_dapp_url", () => invoke("minimal_host_test_dapp_url"));
  phantomSteps.push(urlStep);
  if (urlStep.ok) {
    testDappUrlEl.value = urlStep.result;
    phantomSteps.push(
      await step("open_test_dapp_window", () =>
        invoke("minimal_host_open_test_dapp_window", { url: urlStep.result }),
      ),
    );
    log("waiting for test-dapp probe...");
  }

  const probeStep = await step("read_probe", async () => {
    // The dapp polls its globals for up to 6s before posting, and the post
    // itself takes a beat — poll the slot rather than racing a fixed sleep.
    let probe = null;
    const deadline = Date.now() + 14000;
    while (Date.now() < deadline) {
      probe = await invoke("minimal_host_probe_dapp");
      if (probe !== null) break;
      await new Promise((r) => setTimeout(r, 500));
    }
    // Null means the dapp page never reported back — the app-origin serving
    // or the record IPC is broken.
    if (probe === null) {
      throw new Error("dapp probe never reported (probe slot still null)");
    }
    // The marketing-asset assertion: Phantom's MAIN-world provider is
    // visible to the page.
    if (!(probe.phantomPresent || probe.solana || probe.isPhantom)) {
      throw new Error(`Phantom provider not visible in dapp: ${asJson(probe)}`);
    }
    // noopPong is data, not a gate: in a multi-extension page the content
    // bootstrap is configured with the FIRST matching extension's identity
    // (Phantom at document_start), so noop's later content script sends
    // under Phantom's id — the single-world approximation documented in
    // runtime/injection.rs. Per-extension worlds are the post-v1 fix.
    if (!probe.noopPong?.ok) {
      log(`noop pong not observed from dapp page (single-world limitation): ${asJson(probe.noopPong)}`, "warn");
    }
    return probe;
  });
  phantomSteps.push(probeStep);
  if (probeStep.ok) phantomProbeEl.textContent = asJson(probeStep.result);

  // Opportunistic: Agent G's plugin-side diagnostics command. If it hasn't
  // landed, Tauri returns "command X not found" and we record that string
  // instead of failing the whole phase.
  phantomSteps.push(
    await step("extensions_diagnostics", () => invoke("plugin:extensions|extensions_diagnostics")),
  );

  if (typeof unlisten === "function") {
    try {
      await unlisten();
    } catch (e) {
      log(`unlisten failed: ${e instanceof Error ? e.message : e}`, "warn");
    }
  }

  phantomLifecycleEl.textContent = asJson(lifecycleEvents);

  report.steps.push(...phantomSteps);
  report.phantom = {
    id: phantomId,
    probe: probeStep.ok ? probeStep.result : null,
    lifecycleEvents,
  };

  log(`─ phantom phase end (${phantomSteps.filter((s) => s.ok).length}/${phantomSteps.length} ok) ─`);
}

// --- BG-host fixtures (D-008) ---
// The faithful background-service-worker host: a hidden webview that loads the
// extension's SW from the resource origin — `import(url)` for module workers,
// a synchronous importScripts shim for classic — instead of inlining it into a
// classic bootstrap. Each fixture sets a `window.__fixtureResult` marker the BG
// introspection reads back; fixture (c) also injects a MAIN-world script onto
// the dapp via chrome.scripting.registerContentScripts after a module import.

/** Pull the loaded id out of a load_unpacked result (string or [string]). */
function loadedIdOf(loadResult) {
  if (!loadResult) return null;
  return typeof loadResult.id === "string" ? loadResult.id : loadResult.id?.[0] ?? null;
}

async function runBgHostFixtures(report) {
  log("─ BG-host fixtures (D-008) begin ─");
  report.bgHost = {};

  // (a) module SW with a top-level import, (b) classic SW using importScripts.
  const simple = [
    {
      name: "module-import-mv3",
      label: "bghost_module_import",
      check: (fr) => {
        if (!fr || fr.kind !== "module-import")
          throw new Error(`module SW did not run (no fixtureResult): ${asJson(fr)}`);
        if (fr.brand !== "module-import-ok")
          throw new Error(`top-level import did not resolve: ${asJson(fr)}`);
        if (fr.sum !== 42) throw new Error(`imported add() wrong: ${asJson(fr)}`);
      },
    },
    {
      name: "classic-importscripts-mv3",
      label: "bghost_classic_importscripts",
      check: (fr) => {
        if (!fr || fr.kind !== "classic-importScripts")
          throw new Error(`classic SW did not run (no fixtureResult): ${asJson(fr)}`);
        if (fr.sum !== 42)
          throw new Error(`importScripts of two files failed (want sum 42): ${asJson(fr)}`);
      },
    },
  ];

  for (const f of simple) {
    const fx = await step(`${f.label}_path`, () =>
      invoke("minimal_host_bg_fixture_path", { name: f.name }),
    );
    report.steps.push(fx);
    if (!fx.ok) continue;
    await unloadAllExtensions();
    const load = await step(`${f.label}_load`, () =>
      invoke("minimal_host_load_unpacked", { path: fx.result }),
    );
    report.steps.push(load);
    const id = load.ok ? loadedIdOf(load.result) : null;
    await new Promise((r) => setTimeout(r, 2200));
    const verify = await step(f.label, async () => {
      if (!id) throw new Error("no loaded id");
      const snap = await readBgSnapshot(id);
      f.check(snap?.fixtureResult);
      return snap.fixtureResult;
    });
    report.steps.push(verify);
    report.bgHost[f.name] = verify.ok ? verify.result : { error: verify.error };
  }

  // (c) module SW that, after importing a chunk, registers a MAIN-world content
  // script — Phantom's EVM-injection shape. Assert both the BG registration and
  // the page-side injection.
  const cName = "module-register-content-mv3";
  const cFx = await step("bghost_register_content_path", () =>
    invoke("minimal_host_bg_fixture_path", { name: cName }),
  );
  report.steps.push(cFx);
  if (cFx.ok) {
    await unloadAllExtensions();
    const load = await step("bghost_register_content_load", () =>
      invoke("minimal_host_load_unpacked", { path: cFx.result }),
    );
    report.steps.push(load);
    const id = load.ok ? loadedIdOf(load.result) : null;
    await new Promise((r) => setTimeout(r, 2500));

    const bgStep = await step("bghost_register_content_bg", async () => {
      if (!id) throw new Error("no loaded id");
      const snap = await readBgSnapshot(id);
      const fr = snap?.fixtureResult;
      if (!fr || fr.kind !== "module-register-content")
        throw new Error(`module SW did not run: ${asJson(snap)}`);
      if (!fr.registered)
        throw new Error(`registerContentScripts did not resolve: ${asJson(fr)}`);
      return fr;
    });
    report.steps.push(bgStep);

    const urlStep = await step("bghost_register_content_dapp_url", () =>
      invoke("minimal_host_test_dapp_url"),
    );
    report.steps.push(urlStep);
    if (urlStep.ok) {
      const pageStep = await step("bghost_register_content_page", async () => {
        await invoke("minimal_host_open_test_dapp_window", { url: urlStep.result });
        await new Promise((r) => setTimeout(r, 1300));
        const back = await invoke("minimal_host_introspect_dapp");
        if (!back || !back.mainWorldFixture)
          throw new Error(`registered MAIN-world inpage not on dapp page: ${asJson(back)}`);
        return back;
      });
      report.steps.push(pageStep);
      report.bgHost[cName] = {
        bg: bgStep.ok ? bgStep.result : { error: bgStep.error },
        page: pageStep.ok ? { mainWorldFixture: true } : { error: pageStep.error },
      };
    }
  }
  log("─ BG-host fixtures end ─");
}

// --- EVM provider matrix ---
// For each EVM wallet, in isolation (one loaded at a time so window.ethereum
// is unambiguous): load it, let its BG spin up, introspect the BG's error +
// api-gap buffers, open the canary, and read back the per-wallet provider
// probe. Results are recorded as report.evm — diagnostic data, not gates, so
// the noop + Phantom-Solana regression steps still decide report.ok.

async function unloadAllExtensions() {
  try {
    const list = await invoke("plugin:extensions|extensions_list");
    if (Array.isArray(list)) {
      for (const summary of list) {
        const id = typeof summary.id === "string" ? summary.id : summary.id?.[0];
        if (id) {
          try { await invoke("plugin:extensions|extensions_unload", { id }); } catch { /* best effort */ }
        }
      }
    }
  } catch (e) {
    log(`unloadAll failed: ${e instanceof Error ? e.message : e}`, "warn");
  }
}

async function readBgSnapshot(walletId) {
  // introspect_bg evals an IIFE that posts its snapshot back through the same
  // ProbeSlot the dapp uses — so read it BEFORE opening the dapp window.
  try {
    await invoke("minimal_host_introspect_bg", { extensionId: walletId });
  } catch (e) {
    return { introspectError: e instanceof Error ? e.message : String(e) };
  }
  await new Promise((r) => setTimeout(r, 800));
  try {
    return await invoke("minimal_host_probe_dapp");
  } catch (e) {
    return { probeReadError: e instanceof Error ? e.message : String(e) };
  }
}

async function readDappProbe(urlResult) {
  // Beacon-slot snapshot BEFORE opening the page: lets us attribute hits that
  // arrive during page load (static <img> fires at parse time, page-script
  // beacons at script time) to THIS page, even though introspect_dapp later
  // clears the slot.
  let beaconBefore = null;
  try { beaconBefore = await invoke("minimal_host_read_report"); } catch { /* diag only */ }
  await invoke("minimal_host_open_test_dapp_window", { url: urlResult });
  await new Promise((r) => setTimeout(r, 4000));
  let beaconAfterLoad = null;
  try { beaconAfterLoad = await invoke("minimal_host_read_report"); } catch { /* diag only */ }
  let probe = null;
  const deadline = Date.now() + 12000;
  while (Date.now() < deadline) {
    probe = await invoke("minimal_host_probe_dapp");
    if (probe !== null && !probe.dappIntrospect) break;
    await new Promise((r) => setTimeout(r, 500));
  }
  // Authoritative readback via the document.title channel: a wallet's
  // MAIN-world inpage script can break the page's Tauri IPC entirely
  // (MetaMask's SES lockdown does), so neither the page probe nor an
  // invoke-based readback reports. introspect_dapp reads window.ethereum and
  // returns the snapshot directly via document.title. It wins on
  // provider-presence fields.
  let evalSnap = null;
  try {
    const back = await invoke("minimal_host_introspect_dapp");
    if (back && back.dappIntrospect) evalSnap = back;
    else if (back) evalSnap = back; // keep timeout diagnostics (windowUrl)
  } catch { /* best effort */ }
  const beaconDiag = { beaconBefore, beaconAfterLoad };
  if (evalSnap) {
    return Object.assign({}, probe || {}, evalSnap, { pageProbe: probe, beaconDiag });
  }
  return Object.assign({}, probe || {}, { beaconDiag });
}

async function runEvmMatrix(report) {
  log("─ EVM provider matrix begin ─");
  const wallets = [
    { name: "metamask", cmd: "minimal_host_metamask_fixture_path" },
    { name: "rabby", cmd: "minimal_host_rabby_fixture_path" },
    { name: "phantom", cmd: "minimal_host_phantom_fixture_path" },
  ];

  const urlStep = await step("evm_test_dapp_url", () => invoke("minimal_host_test_dapp_url"));
  const dappUrl = urlStep.ok ? urlStep.result : "test-dapp.html";

  report.evm = {};
  for (const wallet of wallets) {
    const fixture = await step(`${wallet.name}_fixture_path`, () => invoke(wallet.cmd));
    report.steps.push(fixture);
    if (!fixture.ok) {
      log(`${wallet.name} fixture unavailable — skipping`, "warn");
      report.evm[wallet.name] = { skipped: true, reason: fixture.error };
      continue;
    }

    // Isolation: only this wallet loaded so window.ethereum is unambiguous.
    await unloadAllExtensions();

    const loadStep = await step(`${wallet.name}_load`, () =>
      invoke("minimal_host_load_unpacked", { path: fixture.result }),
    );
    report.steps.push(loadStep);
    const walletId = loadStep.ok
      ? typeof loadStep.result.id === "string"
        ? loadStep.result.id
        : loadStep.result.id?.[0] ?? null
      : null;

    // Let the BG service worker boot.
    await new Promise((r) => setTimeout(r, 2500));

    // Wallet onboarding (test-harness setup, NOT a runtime feature): a fresh
    // wallet gates dapp RPC until the user has created a wallet. Rabby gates ALL
    // provider RPC on `hasVault()` === `!!keyringState.vault`. Seed a keyring
    // vault into its chrome.storage.local (the persisted state a real user
    // reaches after creating/importing a wallet), then reload so the service
    // worker re-reads it on boot and `hasVault()` returns true. eth_chainId then
    // returns the controller's chain (0x1) without needing the vault decrypted.
    // The runtime stays wallet-agnostic; this is harness-only state setup.
    if (wallet.name === "rabby" && walletId && fixture.ok) {
      const seed = await step("rabby_seed_vault", async () => {
        await invoke("plugin:extensions|extensions_storage_set", {
          extensionId: walletId,
          area: "local",
          items: {
            keyringState: {
              booted: '{"data":"mv3","iv":"mv3","salt":"mv3"}',
              vault: '{"data":"mv3-seeded","iv":"mv3","salt":"mv3"}',
            },
          },
        });
        // Reload so the SW re-reads keyringState on its next boot.
        await invoke("minimal_host_load_unpacked", { path: fixture.result });
        await new Promise((r) => setTimeout(r, 2800));
        return { seeded: true };
      });
      report.steps.push(seed);
      log(`rabby seed_vault: ${asJson(seed.result || seed.error)}`);
    }

    const bgSnapshot = walletId ? await readBgSnapshot(walletId) : { noId: true };
    const probe = await step(`${wallet.name}_probe`, () => readDappProbe(dappUrl));
    report.steps.push(probe);

    report.evm[wallet.name] = {
      id: walletId,
      bg: bgSnapshot,
      probe: probe.ok ? probe.result : null,
    };
    const p = probe.ok ? probe.result : null;
    log(
      `${wallet.name}: ethereum=${p?.ethereum} isMetaMask=${p?.ethereumIsMetaMask} ` +
        `isRabby=${p?.ethereumIsRabby} phantomEthereum=${p?.phantomEthereum} ` +
        `eip6963=${p?.eip6963Count} chainId=${asJson(p?.chainId)}`,
      p?.ethereum || p?.phantomEthereum || p?.eip6963Count ? "ok" : "warn",
    );
  }

  log("─ EVM provider matrix end ─");
}

// Kick off on load — but only in the real main window. The plugin's hidden
// per-extension BG webviews load the app origin's root document (this same
// page!), so without this guard every loaded extension spawns a concurrent
// acceptance run of its own — racing loads, duplicate test-dapp windows,
// and clobbered reports.
function currentWebviewLabel() {
  const meta = /** @type {any} */ (window).__TAURI_INTERNALS__?.metadata;
  return meta?.currentWebview?.label ?? meta?.currentWindow?.label ?? null;
}

window.addEventListener("DOMContentLoaded", () => {
  const label = currentWebviewLabel();
  if (label !== null && label !== "main") {
    log(`acceptance skipped — running in webview '${label}', not 'main'`);
    return;
  }
  log("minimal-host frontend ready");
  // Small delay lets the plugin's setup closure finish (BackendState, etc.)
  // before we start hitting the IPC surface.
  setTimeout(runAcceptance, 200);
});
