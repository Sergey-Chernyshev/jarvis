import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type JarvisCoreTransport = Readonly<{
  invoke: typeof invoke;
  listen: typeof listen;
  getCurrentWindow: typeof getCurrentWindow;
}>;

declare global {
  // The trusted bridge consumes and deletes this bootstrap reference.
  // Plugin assets are served from a separate package index and never receive it.
  var __JARVIS_CORE_TRANSPORT__: JarvisCoreTransport | undefined;
}

// getCurrentWindow backs the window-mode traffic lights (minimize/zoom/close/
// fullscreen). It is exposed here rather than reached through window.__TAURI__
// so the trusted surface stays declared in one auditable place.
globalThis.__JARVIS_CORE_TRANSPORT__ = Object.freeze({
  invoke,
  listen,
  getCurrentWindow,
});
