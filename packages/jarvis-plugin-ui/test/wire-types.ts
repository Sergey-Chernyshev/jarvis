import type {
  BridgeClientFrame,
  BridgeHostFrame,
  CommandResult,
  ContractRef,
  EntityMutation,
  ResolvedContributions,
  SettingValue,
} from "../src/generated/contracts.js";

export const contractRef: ContractRef = {
  id: "dev.example/runtime",
  schemaDigest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  version: "1.2.3-alpha.1+build.7",
};

export const bridgeRequest: BridgeClientFrame = {
  deadlineMs: 10_000,
  generation: 7,
  id: "request/01",
  method: "entities.watch",
  namespace: "broker",
  params: { contract: "dev.example/runtime@1.0.0" },
  type: "request",
  v: 1,
};

export const bridgeError: BridgeHostFrame = {
  code: "grant_scope_denied",
  correlationId: "correlation/01",
  generation: 7,
  id: "request/01",
  type: "error",
  v: 1,
};

export const entityMutation: EntityMutation = {
  contract: contractRef,
  data: { status: "running" },
  expectedRevision: 4,
  id: "runtime/01",
  type: "put",
};

export const commandResult: CommandResult = {
  operationRef: "runtime-operation/01",
  type: "accepted",
};

export const sensitiveValue: SettingValue = {
  reference: { credentialId: "credential/01" },
  type: "credentialReference",
};

export const emptyContributions: ResolvedContributions = {
  actions: [],
  commands: [],
  hotkeys: [],
  pages: [],
};

export const spoofedRequest: BridgeClientFrame = {
  deadlineMs: 10_000,
  generation: 7,
  id: "request/02",
  method: "entities.query",
  namespace: "broker",
  params: {},
  // @ts-expect-error Caller identity is host-bound and never accepted from plugin UI.
  pluginId: "dev.victim",
  type: "request",
  v: 1,
};
