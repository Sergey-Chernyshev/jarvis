import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

type JarvisCoreTransport = Readonly<{
  invoke: typeof invoke;
  listen: typeof listen;
}>;

declare global {
  // The trusted bridge consumes and deletes this bootstrap reference.
  // Plugin assets are served from a separate package index and never receive it.
  var __JARVIS_CORE_TRANSPORT__: JarvisCoreTransport | undefined;
}

globalThis.__JARVIS_CORE_TRANSPORT__ = Object.freeze({ invoke, listen });
