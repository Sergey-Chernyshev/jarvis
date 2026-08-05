import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { parse } from "parse5";

const CORE_WEBVIEWS = ["main", "toast", "onboarding", "agent-chat"];
const CORE_WEBVIEW_SET = new Set(CORE_WEBVIEWS);
const EXPECTED_CSP =
  "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self' ipc: http://ipc.localhost";
const TRANSPORT_SCRIPT = "./generated/tauri-transport.js";
const TRUSTED_DOCUMENTS = new Map([
  ["main", { filename: "index.html", consumer: "./bridge.js" }],
  ["toast", { filename: "toast.html", consumer: "./toast-bridge.js" }],
  [
    "onboarding",
    {
      filename: "onboarding.html",
      consumer: "onboarding.js",
      predecessor: "onboarding-state.js",
    },
  ],
  ["agent-chat", { filename: "agent-chat.html", consumer: "agent-chat.js" }],
]);

export class TauriAclError extends Error {
  constructor(code, detail = "") {
    super(detail ? `${code}: ${detail}` : code);
    this.name = "TauriAclError";
    this.code = code;
  }
}

function fail(code, detail) {
  throw new TauriAclError(code, detail);
}

function foldAsciiCodeUnit(codeUnit) {
  return codeUnit >= 65 && codeUnit <= 90 ? codeUnit + 32 : codeUnit;
}

function foldAsciiString(source) {
  let folded = "";
  for (let index = 0; index < source.length; index += 1) {
    folded += String.fromCharCode(foldAsciiCodeUnit(source.charCodeAt(index)));
  }
  return folded;
}

function asciiCaseInsensitiveEqualAt(source, offset, token) {
  if (offset < 0 || offset + token.length > source.length) return false;
  for (let index = 0; index < token.length; index += 1) {
    if (
      foldAsciiCodeUnit(source.charCodeAt(offset + index)) !==
      foldAsciiCodeUnit(token.charCodeAt(index))
    ) {
      return false;
    }
  }
  return true;
}

function indexOfAsciiCaseInsensitive(source, token, fromIndex) {
  const lastStart = source.length - token.length;
  for (let offset = Math.max(0, fromIndex); offset <= lastStart; offset += 1) {
    if (asciiCaseInsensitiveEqualAt(source, offset, token)) return offset;
  }
  return -1;
}

export function parseClassicExternalScripts(
  source,
  relative = "trusted document",
) {
  const invalid = (detail) =>
    fail("tauri_acl_script_tag_invalid", `${relative}: ${detail}`);
  const parseErrors = [];
  const document = parse(source, {
    scriptingEnabled: true,
    sourceCodeLocationInfo: true,
    onParseError(error) {
      parseErrors.push(error.code);
    },
  });
  const blockingParseError = parseErrors.find(
    (code) => code !== "invalid-first-character-of-tag-name",
  );
  if (blockingParseError) {
    invalid(`html parse error ${blockingParseError}`);
  }

  const html = document.childNodes.find((node) => node.nodeName === "html");
  const body = html?.childNodes?.find((node) => node.nodeName === "body");
  const htmlLocation = html?.sourceCodeLocation;
  const bodyLocation = body?.sourceCodeLocation;
  if (!htmlLocation?.startTag || !htmlLocation.endTag) {
    invalid("explicit html boundary required");
  }
  if (!body || !bodyLocation?.startTag || !bodyLocation.endTag) {
    invalid("explicit body boundary required");
  }

  const allScriptOffsets = new Set();
  const scripts = [];
  const visit = (node, parent) => {
    if (node.nodeName === "script") {
      const location = node.sourceCodeLocation;
      if (!location?.startTag || !location.endTag) {
        invalid("script source location missing");
      }
      allScriptOffsets.add(location.startTag.startOffset);
      if (
        node.namespaceURI !== "http://www.w3.org/1999/xhtml" ||
        parent !== body ||
        location.startTag.startOffset < bodyLocation.startTag.endOffset ||
        location.endTag.endOffset > bodyLocation.endTag.startOffset
      ) {
        invalid("script must be a direct body child");
      }
      if (
        !Array.isArray(node.attrs) ||
        node.attrs.length !== 1 ||
        node.attrs[0].name !== "src" ||
        node.attrs[0].namespace != null ||
        node.attrs[0].prefix != null ||
        node.attrs[0].value.length === 0
      ) {
        invalid("script must have exactly one non-empty src attribute");
      }
      const rawStartTag = source.slice(
        location.startTag.startOffset,
        location.startTag.endOffset,
      );
      if (
        !/^<script[ \t\n\r\f]+src[ \t\n\r\f]*=[ \t\n\r\f]*(?:"[^"]+"|'[^']+')[ \t\n\r\f]*>$/.test(
          foldAsciiString(rawStartTag),
        )
      ) {
        invalid("script src must be the only quoted attribute");
      }
      if (
        (node.childNodes ?? []).some(
          (child) =>
            child.nodeName !== "#text" ||
            String(child.value ?? "").trim().length > 0,
        )
      ) {
        invalid("inline script body");
      }
      scripts.push(Object.freeze({ src: node.attrs[0].value }));
    }

    for (const child of node.childNodes ?? []) {
      visit(child, node);
    }
    if (node.nodeName === "template" && node.content) {
      visit(node.content, node);
    }
  };
  visit(document, null);

  const rawScriptOffsets = [];
  let cursor = 0;
  while (cursor < source.length) {
    const start = indexOfAsciiCaseInsensitive(source, "<script", cursor);
    if (start === -1) break;
    const boundary = source[start + "<script".length];
    if (
      boundary === undefined ||
      boundary === " " ||
      boundary === "\t" ||
      boundary === "\n" ||
      boundary === "\r" ||
      boundary === "\f" ||
      boundary === ">" ||
      boundary === "/"
    ) {
      rawScriptOffsets.push(start);
    }
    cursor = start + "<script".length;
  }
  if (
    rawScriptOffsets.length !== allScriptOffsets.size ||
    rawScriptOffsets.some((offset) => !allScriptOffsets.has(offset))
  ) {
    invalid("script token is inert, malformed, or hidden from the DOM");
  }

  return Object.freeze(scripts);
}

function readJson(root, relative) {
  const file = path.join(root, relative);
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch (error) {
    fail("tauri_acl_json_invalid", `${relative}: ${error.message}`);
  }
}

function parseInventory(root) {
  const relative = "src-tauri/src/app_command_inventory.rs";
  const source = readFileSync(path.join(root, relative), "utf8");
  const row =
    /\(\s*"([a-z][a-z0-9_]*)"\s*,\s*crate::(?:ipc|onboarding)::([a-z][a-z0-9_]*)\s*,\s*\[([^\]]*)\]\s*\)/g;
  const label = /"([^"]+)"/g;
  const inventory = new Map();

  for (const match of source.matchAll(row)) {
    const [, command, handler, rawLabels] = match;
    if (command !== handler || inventory.has(command)) {
      fail("tauri_acl_inventory_invalid", command);
    }
    const webviews = new Set(
      [...rawLabels.matchAll(label)].map((entry) => entry[1]),
    );
    if (
      webviews.size !== [...rawLabels.matchAll(label)].length ||
      [...webviews].some((webview) => !CORE_WEBVIEW_SET.has(webview))
    ) {
      fail("tauri_acl_inventory_invalid", command);
    }
    inventory.set(command, webviews);
  }

  if (inventory.size === 0) {
    fail("tauri_acl_inventory_invalid", "empty command inventory");
  }
  return inventory;
}

function sameSet(left, right) {
  return (
    left.size === right.size && [...left].every((entry) => right.has(entry))
  );
}

function generatedPermission(command) {
  const slug = command.replaceAll("_", "-");
  return `# Automatically generated - DO NOT EDIT!

[[permission]]
identifier = "allow-${slug}"
description = "Enables the ${command} command without any pre-configured scope."
commands.allow = ["${command}"]

[[permission]]
identifier = "deny-${slug}"
description = "Denies the ${command} command without any pre-configured scope."
commands.deny = ["${command}"]
`;
}

function checkGeneratedPermissionBoundary(root, inventory) {
  const requiredIgnore = "/src-tauri/permissions/autogenerated/";
  const ignoreRules = readFileSync(path.join(root, ".gitignore"), "utf8")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
  const permissionRules = ignoreRules.filter((rule) =>
    rule.replace(/^!/, "").includes("src-tauri/permissions"),
  );
  if (permissionRules.length !== 1 || permissionRules[0] !== requiredIgnore) {
    fail("tauri_acl_generated_permission_ignore_invalid");
  }
  const ignored = (relative) => {
    const result = spawnSync(
      "git",
      ["check-ignore", "--quiet", "--no-index", "--", relative],
      { cwd: root, stdio: "ignore" },
    );
    if (result.status !== 0 && result.status !== 1) {
      fail("tauri_acl_generated_permission_ignore_invalid", "git probe");
    }
    return result.status === 0;
  };
  if (
    !ignored("src-tauri/permissions/autogenerated/__probe__.toml") ||
    ignored("src-tauri/permissions/manual.toml")
  ) {
    fail("tauri_acl_generated_permission_ignore_invalid", "scope");
  }

  const permissionsRoot = path.join(root, "src-tauri/permissions");
  if (!existsSync(permissionsRoot)) return;

  const rootEntries = readdirSync(permissionsRoot, { withFileTypes: true });
  if (
    rootEntries.some(
      (entry) => entry.name !== "autogenerated" || !entry.isDirectory(),
    )
  ) {
    fail("tauri_acl_handwritten_permission_forbidden");
  }

  const autogenerated = path.join(permissionsRoot, "autogenerated");
  if (!existsSync(autogenerated)) return;

  const expectedFiles = new Set(
    [...inventory.keys()].map((command) => `${command}.toml`),
  );
  const entries = readdirSync(autogenerated, { withFileTypes: true });
  const actualFiles = new Set(entries.map((entry) => entry.name));
  if (
    entries.some((entry) => !entry.isFile()) ||
    !sameSet(actualFiles, expectedFiles)
  ) {
    fail("tauri_acl_generated_permission_drift", "file set");
  }
  for (const command of inventory.keys()) {
    const filename = `${command}.toml`;
    if (
      readFileSync(path.join(autogenerated, filename), "utf8") !==
      generatedPermission(command)
    ) {
      fail("tauri_acl_generated_permission_drift", filename);
    }
  }
}

function attributeValue(attributes, name) {
  const escaped = name.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = attributes.match(
    new RegExp(
      `(?:^|\\s)${escaped}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s"'=<>]+))`,
      "i",
    ),
  );
  return match ? (match[1] ?? match[2] ?? match[3]) : undefined;
}

function hasAttribute(attributes, name) {
  const escaped = name.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|\\s)${escaped}(?:\\s|=|$)`, "i").test(attributes);
}

function checkTrustedDocuments(root) {
  const draggable = new Set();
  const trafficLit = new Set();

  for (const [identifier, document] of TRUSTED_DOCUMENTS) {
    const relative = path.join("ui", document.filename);
    const source = readFileSync(path.join(root, relative), "utf8").replaceAll(
      /<!--[\s\S]*?-->/g,
      "",
    );
    const cspTags = [...source.matchAll(/<meta\b([^>]*)>/gi)].filter(
      ([, attributes]) =>
        attributeValue(attributes, "http-equiv")?.toLowerCase() ===
        "content-security-policy",
    );
    if (
      cspTags.length !== 1 ||
      attributeValue(cspTags[0][1], "content") !== EXPECTED_CSP
    ) {
      fail("tauri_acl_trusted_html_csp_invalid", relative);
    }

    const scripts = parseClassicExternalScripts(source, relative).map(
      (script) => script.src,
    );
    for (const [, , attributes] of source.matchAll(
      /<(script|link|img|iframe|source|audio|video)\b([^>]*)>/gi,
    )) {
      for (const name of ["src", "href", "srcset"]) {
        const value = attributeValue(attributes, name);
        if (value && /(?:^|[\s,])(?:https?:)?\/\//i.test(value)) {
          fail("tauri_acl_remote_asset_forbidden", relative);
        }
      }
    }

    if (
      scripts.filter((src) => src === TRANSPORT_SCRIPT).length !== 1 ||
      scripts.filter((src) => src === document.consumer).length !== 1
    ) {
      fail("tauri_acl_transport_order_invalid", relative);
    }
    const transportIndex = scripts.indexOf(TRANSPORT_SCRIPT);
    if (
      scripts[transportIndex + 1] !== document.consumer ||
      (document.predecessor &&
        (scripts.filter((src) => src === document.predecessor).length !== 1 ||
          scripts[transportIndex - 1] !== document.predecessor))
    ) {
      fail("tauri_acl_transport_order_invalid", relative);
    }

    for (const [, attributes] of source.matchAll(
      /<[a-z][a-z0-9-]*\b([^>]*)>/gi,
    )) {
      if (hasAttribute(attributes, "data-tauri-drag-region")) {
        draggable.add(identifier);
        break;
      }
    }

    // свой светофор: окно само рисует закрыть/свернуть/развернуть
    if (/\bid="winClose"/.test(source)) {
      trafficLit.add(identifier);
    }
  }

  return { draggable, trafficLit };
}

function readCapabilities(root, enabled) {
  const directory = path.join(root, "src-tauri/capabilities");
  const capabilities = new Map();

  for (const filename of readdirSync(directory).filter((entry) =>
    entry.endsWith(".json"),
  )) {
    const capability = readJson(
      root,
      path.join("src-tauri/capabilities", filename),
    );
    const identifier = capability.identifier;
    if (typeof identifier !== "string") {
      fail("tauri_acl_unlisted_capability_forbidden", filename);
    }
    if (!enabled.has(identifier)) {
      fail("tauri_acl_unlisted_capability_forbidden", identifier);
    }
    if (capabilities.has(identifier)) {
      fail("tauri_acl_capability_duplicate", identifier);
    }
    if (Object.hasOwn(capability, "remote")) {
      fail("tauri_acl_remote_scope_forbidden", identifier);
    }
    if (capability.local !== true) {
      fail("tauri_acl_local_scope_invalid", identifier);
    }
    if (Object.hasOwn(capability, "windows")) {
      fail("tauri_acl_window_scope_forbidden", identifier);
    }
    if (!Array.isArray(capability.webviews)) {
      fail("tauri_acl_webview_scope_missing", identifier);
    }
    for (const webview of capability.webviews) {
      if (
        typeof webview !== "string" ||
        webview.includes("*") ||
        webview.includes("?")
      ) {
        fail("tauri_acl_webview_wildcard_forbidden", identifier);
      }
      if (webview.startsWith("plugin-")) {
        fail("tauri_acl_plugin_webview_forbidden", webview);
      }
    }
    if (
      capability.webviews.length !== 1 ||
      capability.webviews[0] !== identifier
    ) {
      fail("tauri_acl_webview_scope_mismatch", identifier);
    }
    if (!Array.isArray(capability.permissions)) {
      fail("tauri_acl_permissions_missing", identifier);
    }
    if (capability.permissions.includes("core:default")) {
      fail("tauri_acl_core_default_forbidden", identifier);
    }
    capabilities.set(identifier, capability);
  }

  if (!sameSet(new Set(capabilities.keys()), enabled)) {
    fail("tauri_acl_unlisted_capability_forbidden", "enabled/file drift");
  }
  return capabilities;
}

function checkDependencyPins(root) {
  const packageJson = readJson(root, "package.json");
  const dependencies = packageJson.devDependencies ?? {};
  if (
    dependencies["@tauri-apps/api"] !== "2.11.1" ||
    dependencies.esbuild !== "0.25.12" ||
    dependencies.parse5 !== "8.0.0"
  ) {
    fail("tauri_acl_dependency_pin_invalid");
  }
}

function checkConfig(root) {
  const config = readJson(root, "src-tauri/tauri.conf.json");
  const app = config.app ?? {};
  const security = app.security ?? {};

  if (app.withGlobalTauri !== false) {
    fail("tauri_acl_global_tauri_forbidden");
  }
  if (security.csp == null) {
    fail("tauri_acl_csp_missing");
  }
  if (security.csp !== EXPECTED_CSP) {
    fail("tauri_acl_csp_invalid");
  }
  if (security.freezePrototype !== true) {
    fail("tauri_acl_freeze_prototype_missing");
  }
  if (
    !Array.isArray(security.capabilities) ||
    security.capabilities.some((identifier) => typeof identifier !== "string")
  ) {
    fail("tauri_acl_capability_list_invalid");
  }

  const enabled = new Set(security.capabilities);
  if (
    enabled.size !== security.capabilities.length ||
    !sameSet(enabled, CORE_WEBVIEW_SET)
  ) {
    fail("tauri_acl_capability_list_invalid");
  }
  return enabled;
}

function checkGrants(inventory, capabilities, draggable, trafficLit) {
  const actual = new Map();
  for (const command of inventory.keys()) actual.set(command, new Set());

  for (const [identifier, capability] of capabilities) {
    const support = new Set();
    for (const permission of capability.permissions) {
      if (typeof permission !== "string") {
        fail("tauri_acl_permissions_missing", identifier);
      }
      if (!permission.includes(":") && permission.startsWith("allow-")) {
        const command = permission.slice("allow-".length).replaceAll("-", "_");
        if (!actual.has(command)) {
          fail("tauri_acl_command_grant_drift", permission);
        }
        actual.get(command).add(identifier);
      } else {
        support.add(permission);
      }
    }
    const expectedSupport = new Set([
      "core:event:allow-listen",
      "core:event:allow-unlisten",
    ]);
    if (identifier === "main" || identifier === "toast") {
      expectedSupport.add("clipboard-manager:allow-write-text");
    }
    if (draggable.has(identifier)) {
      expectedSupport.add("core:window:allow-start-dragging");
    }
    // оконные операции получает только документ со своим светофором: кнопки
    // существуют в разметке, значит разрешение выводится из неё, а не на веру
    if (trafficLit.has(identifier)) {
      expectedSupport.add("core:window:allow-minimize");
      expectedSupport.add("core:window:allow-toggle-maximize");
      expectedSupport.add("core:window:allow-close");
      expectedSupport.add("core:window:allow-is-fullscreen");
      expectedSupport.add("core:window:allow-set-fullscreen");
    }
    if (!sameSet(support, expectedSupport)) {
      fail("tauri_acl_support_permission_drift", identifier);
    }
  }

  for (const [command, expectedWebviews] of inventory) {
    if (!sameSet(actual.get(command), expectedWebviews)) {
      fail("tauri_acl_command_grant_drift", command);
    }
  }
}

function checkNoGlobalTauri(root) {
  const uiRoot = path.join(root, "ui");
  for (const filename of [
    "bridge.js",
    "toast-bridge.js",
    "onboarding.js",
    "agent-chat.js",
  ]) {
    const source = readFileSync(path.join(uiRoot, filename), "utf8");
    if (source.includes("window.__TAURI__")) {
      fail("tauri_acl_global_tauri_reference_forbidden", filename);
    }
  }
}

export function checkTauriAcl(root = process.cwd()) {
  const repositoryRoot = path.resolve(root);
  checkDependencyPins(repositoryRoot);
  const inventory = parseInventory(repositoryRoot);
  checkGeneratedPermissionBoundary(repositoryRoot, inventory);
  const enabled = checkConfig(repositoryRoot);
  const { draggable, trafficLit } = checkTrustedDocuments(repositoryRoot);
  const capabilities = readCapabilities(repositoryRoot, enabled);
  checkGrants(inventory, capabilities, draggable, trafficLit);
  checkNoGlobalTauri(repositoryRoot);
  return Object.freeze({
    commands: inventory.size,
    capabilities: capabilities.size,
  });
}

const isCli =
  process.argv[1] &&
  fileURLToPath(import.meta.url) === path.resolve(process.argv[1]);
if (isCli) {
  const rootIndex = process.argv.indexOf("--root");
  const root = rootIndex === -1 ? process.cwd() : process.argv[rootIndex + 1];
  if (!root) {
    fail("tauri_acl_root_missing");
  }
  try {
    const result = checkTauriAcl(root);
    process.stdout.write(
      `tauri_acl_ok commands=${result.commands} capabilities=${result.capabilities}\n`,
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
