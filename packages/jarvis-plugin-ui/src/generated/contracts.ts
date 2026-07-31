/* eslint-disable */
/**
 * Generated from Jarvis public plugin JSON Schemas.
 * Do not edit by hand; run `npm run generate:plugin-contracts`.
 */
export type CommandResult =
  | {
      result: unknown;
      type: "completed";
    }
  | {
      operationRef: string;
      type: "accepted";
    };
export type EntityMutation =
  | {
      contract: ContractRef;
      data: unknown;
      expectedRevision: number;
      id: string;
      type: "put";
    }
  | {
      contract: ContractRef;
      expectedRevision: number;
      id: string;
      type: "delete";
    };
export type OutboxMutation =
  | {
      kind: "entity";
      mutation: EntityMutation;
    }
  | {
      event: EventMutation;
      kind: "event";
    };
export type RuntimeOperationState =
  | "queued"
  | "dispatching"
  | "running"
  | "waiting_for_provider"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "timed_out";
export type Risk = "read" | "control" | "destructive";
export type BridgeClientFrame =
  | {
      generation: number;
      type: "hello";
      v: number;
    }
  | {
      deadlineMs: number;
      generation: number;
      id: string;
      method: string;
      namespace: string;
      params: unknown;
      type: "request";
      v: number;
    }
  | {
      cursor: number;
      generation: number;
      type: "poll";
      v: number;
    }
  | {
      generation: number;
      id: string;
      type: "cancel";
      v: number;
    }
  | {
      generation: number;
      subscriptionId: string;
      type: "unsubscribe";
      v: number;
    };
export type BridgeHostFrame =
  | {
      generation: number;
      /**
       * @minItems 0
       * @maxItems 256
       */
      grants: string[];
      packageDigest: string;
      pageId: string;
      pluginId: string;
      type: "welcome";
      v: number;
    }
  | {
      generation: number;
      id: string;
      result: unknown;
      type: "response";
      v: number;
    }
  | {
      cursor: number;
      generation: number;
      id: string;
      subscriptionId: string;
      type: "subscribeResult";
      v: number;
    }
  | {
      cursor: number;
      event: unknown;
      generation: number;
      subscriptionId: string;
      type: "event";
      v: number;
    }
  | {
      earliestCursor: number;
      generation: number;
      latestCursor: number;
      requestedCursor: number;
      subscriptionId: string;
      type: "gap";
      v: number;
    }
  | {
      code: string;
      generation: number;
      type: "close";
      v: number;
    }
  | {
      code: string;
      correlationId?: string | null;
      generation: number;
      id?: string | null;
      message?: string | null;
      type: "error";
      v: number;
    };
export type ContextReference =
  | {
      id: string;
      type: "project";
    }
  | {
      id: string;
      type: "chat";
    }
  | {
      id: string;
      type: "runtime";
    }
  | {
      id: string;
      type: "session";
    };
export type ActionLocation =
  "chat.composer.actions" | "project.actions" | "project.session.context";
export type CommandPlacement = "globalPalette";
export type HotkeyScope = "global";
export type InstancePolicy = "singleton" | "per-project" | "per-session";
export type PagePlacement = "sidebar" | "commandPalette" | "deepLink" | "pluginSettings";
export type SettingScope = "user" | "project";
export type SettingValue =
  | {
      type: "integer";
      value: number;
    }
  | {
      type: "number";
      value: number;
    }
  | {
      type: "boolean";
      value: boolean;
    }
  | {
      type: "string";
      value: string;
    }
  | {
      reference: CredentialReference;
      type: "credentialReference";
    };

export interface JarvisPluginUiContracts {
  broker: BrokerContractV1;
  bridge: BridgeContractV1;
  contributions: ResolvedContributions;
  settings: SettingsContractV1;
}
export interface BrokerContractV1 {
  commandResult: CommandResult;
  contractRef: ContractRef;
  cursorGap: CursorGap;
  entityChange: EntityChange;
  entityEnvelope: EntityEnvelope;
  entityMutation: EntityMutation;
  entityQuery: EntityQuery;
  entityQuerySnapshot: EntityQuerySnapshot;
  entitySelector: EntitySelector;
  entityWatchRequest: EntityWatchRequest;
  eventChange: EventChange;
  eventEnvelope: EventEnvelope;
  eventMutation: EventMutation;
  eventWatchRequest: EventWatchRequest;
  fieldProjection: FieldProjection;
  operationSubjectRef: OperationSubjectRef;
  outboxAck: OutboxAck;
  outboxBatch: OutboxBatch;
  outboxMutation: OutboxMutation;
  runtimeOperationCancel: RuntimeOperationCancel;
  runtimeOperationChange: RuntimeOperationChange;
  runtimeOperationGap: RuntimeOperationGap;
  runtimeOperationQuery: RuntimeOperationQuery;
  runtimeOperationView: RuntimeOperationView;
  runtimeOperationWatch: RuntimeOperationWatch;
  typedCommandDeclaration: TypedCommandDeclaration;
  typedCommandInvocation: TypedCommandInvocation;
}
export interface ContractRef {
  id: string;
  schemaDigest: string;
  version: string;
}
export interface CursorGap {
  earliestCursor: number;
  latestCursor: number;
  requestedCursor: number;
}
export interface EntityChange {
  cursor: number;
  entity: EntityEnvelope;
}
export interface EntityEnvelope {
  brokerRevision: number;
  contract: ContractRef;
  data: unknown;
  id: string;
  revision: number;
  stale: boolean;
  state: string;
  updatedAtMs: number;
}
export interface EntityQuery {
  limit: number;
  projection?: FieldProjection | null;
  selectors: EntitySelector[];
}
export interface FieldProjection {
  /**
   * @minItems 1
   * @maxItems 64
   */
  fields: string[];
}
export interface EntitySelector {
  contract: ContractRef;
  /**
   * @minItems 0
   * @maxItems 128
   */
  ids?: string[];
  /**
   * @minItems 0
   * @maxItems 128
   */
  states?: string[];
}
export interface EntityQuerySnapshot {
  entities: EntityEnvelope[];
  snapshotRevision: number;
}
export interface EntityWatchRequest {
  cursor: number;
  limit: number;
  projection?: FieldProjection | null;
  selectors: EntitySelector[];
}
export interface EventChange {
  cursor: number;
  event: EventEnvelope;
}
export interface EventEnvelope {
  atMs: number;
  contract: ContractRef;
  correlationId?: string | null;
  data: unknown;
  eventId: string;
  kind: string;
  seq: number;
  streamId: string;
  subject: string;
}
export interface EventMutation {
  atMs: number;
  contract: ContractRef;
  correlationId?: string | null;
  data: unknown;
  eventId: string;
  kind: string;
  streamId: string;
  subject: string;
}
export interface EventWatchRequest {
  contract: ContractRef;
  cursor: number;
  limit: number;
  /**
   * @minItems 0
   * @maxItems 128
   */
  subjects?: string[];
}
export interface OperationSubjectRef {
  contract: ContractRef;
  subjectId: string;
}
export interface OutboxAck {
  /**
   * @maxItems 128
   */
  acceptedOperationRefs: string[];
  appliedBrokerRevision: number;
  outboxId: string;
  payloadDigest: string;
  sourceInstanceId: string;
}
export interface OutboxBatch {
  mutations: OutboxMutation[];
  outboxId: string;
  sourceInstanceId: string;
}
export interface RuntimeOperationCancel {
  expectedStateRevision: number;
  operationRef: string;
}
export interface RuntimeOperationChange {
  cursor: number;
  operation: RuntimeOperationView;
}
export interface RuntimeOperationView {
  createdAt: number;
  deadlineAt: number;
  error?: RuntimeOperationError | null;
  exactCommand: ContractRef;
  operationRef: string;
  phase: string;
  providerGeneration: number;
  state: RuntimeOperationState;
  subject: OperationSubjectRef;
  updatedAt: number;
}
export interface RuntimeOperationError {
  code: string;
  message?: string | null;
}
export interface RuntimeOperationGap {
  earliestCursor: number;
  latestCursor: number;
  requestedCursor: number;
}
export interface RuntimeOperationQuery {
  includeTerminalSince?: number | null;
  limit: number;
  subjects: OperationSubjectRef[];
}
export interface RuntimeOperationWatch {
  cursor: number;
  limit: number;
  subjects: OperationSubjectRef[];
}
export interface TypedCommandDeclaration {
  command: ContractRef;
  riskFloor: Risk;
}
export interface TypedCommandInvocation {
  args: unknown;
  command: ContractRef;
  deadlineMs: number;
  subject: OperationSubjectRef;
}
export interface BridgeContractV1 {
  clientFrame: BridgeClientFrame;
  hostFrame: BridgeHostFrame;
}
export interface ResolvedContributions {
  actions?: ResolvedActionContribution[];
  commands?: ResolvedCommandContribution[];
  hotkeys?: ResolvedHotkeyContribution[];
  pages?: ResolvedPageContribution[];
}
export interface ResolvedActionContribution {
  command: string;
  context?: ContextReference[];
  id: string;
  locations: ActionLocation[];
  riskFloor: Risk;
  title: string;
}
export interface ResolvedCommandContribution {
  context?: ContextReference[];
  id: string;
  placements: CommandPlacement[];
  riskFloor: Risk;
  title: string;
}
export interface ResolvedHotkeyContribution {
  command: string;
  scope: HotkeyScope;
  shortcut: string;
}
export interface ResolvedPageContribution {
  id: string;
  instancePolicy: InstancePolicy;
  placements: PagePlacement[];
  title: string;
}
export interface SettingsContractV1 {
  change: SettingChange;
  record: SettingRecord;
  value: SettingValue;
  write: SettingWrite;
}
export interface SettingChange {
  cursor: number;
  setting: SettingRecord;
}
export interface SettingRecord {
  key: string;
  projectId?: string | null;
  revision: number;
  scope: SettingScope;
  value: SettingValue;
}
export interface CredentialReference {
  credentialId: string;
}
export interface SettingWrite {
  expectedRevision: number;
  key: string;
  projectId?: string | null;
  scope: SettingScope;
  value: SettingValue;
}
