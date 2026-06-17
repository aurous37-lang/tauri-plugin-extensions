// Assemble the chrome.* namespace and install it on `globalThis` (and
// `window` when it exists). Returns the installed object so the bootstrap
// can keep a reference for BG-surface synthetic events.

import { createRuntime } from "./runtime.js";
import { createStorage } from "./storage.js";
import {
  createAction,
  createAlarms,
  createI18n,
  createIdentity,
  createIdle,
  createManagement,
  createNotifications,
  createOffscreen,
  createPermissions,
  createScripting,
  createTabs,
  createWebNavigation,
  createWebRequest,
  createWindows,
} from "./stubs.js";

export interface ChromeNamespace {
  runtime: ReturnType<typeof createRuntime>;
  storage: ReturnType<typeof createStorage>;
  action: ReturnType<typeof createAction>;
  scripting: ReturnType<typeof createScripting>;
  i18n: ReturnType<typeof createI18n>;
  permissions: ReturnType<typeof createPermissions>;
  management: ReturnType<typeof createManagement>;
  tabs: ReturnType<typeof createTabs>;
  alarms: ReturnType<typeof createAlarms>;
  offscreen: ReturnType<typeof createOffscreen>;
  windows: ReturnType<typeof createWindows>;
  webNavigation: ReturnType<typeof createWebNavigation>;
  idle: ReturnType<typeof createIdle>;
  notifications: ReturnType<typeof createNotifications>;
  identity: ReturnType<typeof createIdentity>;
  webRequest: ReturnType<typeof createWebRequest>;
  extension: {
    getURL: (path: string) => string;
    getBackgroundPage: () => null;
    isAllowedIncognitoAccess: () => Promise<boolean>;
    inIncognitoContext: boolean;
  };
}

export function installChrome(): ChromeNamespace {
  const runtime = createRuntime();
  const storage = createStorage();
  const action = createAction();
  const scripting = createScripting();
  const i18n = createI18n();
  const permissions = createPermissions();
  // Survival stubs for namespaces wallet BG workers touch during init (absent
  // before → first access threw and aborted the worker). See stubs.ts.
  const management = createManagement();
  const tabs = createTabs();
  const alarms = createAlarms();
  const offscreen = createOffscreen();
  const windows = createWindows();
  const webNavigation = createWebNavigation();
  const idle = createIdle();
  const notifications = createNotifications();
  const identity = createIdentity();
  const webRequest = createWebRequest();

  // chrome.extension is a legacy alias surface — MV3 deprecated most of
  // it, but `getURL` and `inIncognitoContext` are still commonly touched
  // in shared utils files inside wallet extensions.
  const extension = {
    getURL: (path: string) => runtime.getURL(path),
    getBackgroundPage: () => null,
    isAllowedIncognitoAccess: () => Promise.resolve(false),
    inIncognitoContext: false,
  };

  const ns: ChromeNamespace = {
    runtime,
    storage,
    action,
    scripting,
    i18n,
    permissions,
    management,
    tabs,
    alarms,
    offscreen,
    windows,
    webNavigation,
    idle,
    notifications,
    identity,
    webRequest,
    extension,
  };

  // Expose on globalThis. Both `chrome` and `browser` (WebExtensions alias)
  // point at the same object; wallet extensions feature-detect either.
  const g = globalThis as unknown as {
    chrome?: unknown;
    browser?: unknown;
  };

  // Only install if an existing chrome object doesn't already own the
  // fields. This keeps us safe if a future Tauri release exposes a native
  // chrome namespace we don't want to overwrite.
  if (!g.chrome || typeof g.chrome !== "object") {
    g.chrome = ns;
  } else {
    Object.assign(g.chrome as object, ns);
  }
  if (!g.browser) {
    g.browser = g.chrome;
  }

  return ns;
}
