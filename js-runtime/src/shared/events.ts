// Minimal event-emitter helper modelled after Chrome's `Event` interface
// (addListener / removeListener / hasListener / hasListeners). The real
// Chrome `chrome.events.Event` is implemented natively in C++ and carries
// extra surface (addRules, etc.) that extensions rarely use and that the
// spike explicitly skips.
//
// Listeners are stored in insertion order and invoked synchronously in
// that order. Errors thrown by a listener are logged but do not prevent
// subsequent listeners from running — matching Chrome's behaviour.

export type Listener<Args extends unknown[]> = (...args: Args) => unknown;

export class ChromeEvent<Args extends unknown[]> {
  readonly #listeners: Array<Listener<Args>> = [];
  // When buffering is on, events emitted while there are NO listeners are
  // queued and replayed to the first listener that registers. This emulates
  // Chrome's service-worker semantics, where an event (e.g. onConnect, a port
  // message) starts the worker and is delivered AFTER its top-level listeners
  // register — in this document host the extension's controller may register
  // its listeners asynchronously (after an importScripts'd controller boots),
  // so an early connect/message would otherwise be dropped on the floor and the
  // wallet's provider handshake would hang. Off by default (most events should
  // drop when unhandled, matching Chrome).
  readonly #bufferWhenEmpty: boolean;
  #buffer: Args[] = [];

  constructor(bufferWhenEmpty = false) {
    this.#bufferWhenEmpty = bufferWhenEmpty;
  }

  addListener(cb: Listener<Args>): void {
    if (typeof cb !== "function") {
      throw new TypeError("addListener: callback must be a function");
    }
    if (!this.#listeners.includes(cb)) {
      this.#listeners.push(cb);
    }
    // First listener drains anything buffered before it existed.
    if (this.#buffer.length > 0) {
      const queued = this.#buffer;
      this.#buffer = [];
      for (const args of queued) {
        try {
          cb(...args);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.error("[chrome-shim] buffered listener threw:", err);
        }
      }
    }
  }

  removeListener(cb: Listener<Args>): void {
    const idx = this.#listeners.indexOf(cb);
    if (idx >= 0) this.#listeners.splice(idx, 1);
  }

  hasListener(cb: Listener<Args>): boolean {
    return this.#listeners.includes(cb);
  }

  hasListeners(): boolean {
    return this.#listeners.length > 0;
  }

  /// Fire all listeners; returns the array of (non-undefined) return values
  /// so callers can decide whether any listener claimed the message (the
  /// Chrome contract for `onMessage`: returning `true` keeps the channel
  /// open; returning a promise resolves to `sendResponse`).
  emit(...args: Args): unknown[] {
    const results: unknown[] = [];
    // Snapshot so removeListener during iteration behaves sanely.
    const snapshot = this.#listeners.slice();
    if (snapshot.length === 0 && this.#bufferWhenEmpty) {
      // No listener yet — queue for replay when one registers (SW semantics).
      this.#buffer.push(args);
      return results;
    }
    for (const listener of snapshot) {
      try {
        results.push(listener(...args));
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[chrome-shim] listener threw:", err);
      }
    }
    return results;
  }
}
