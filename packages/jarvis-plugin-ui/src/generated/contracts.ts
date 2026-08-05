/* eslint-disable */
/**
 * Generated from Jarvis public plugin JSON Schemas.
 * Do not edit by hand; run `npm run generate:plugin-contracts`.
 */
export type CommandResult =
  | {
      /**
       * Serialized JSON size must not exceed 262144 bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords.
       */
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
      /**
       * Serialized JSON size must not exceed 262144 bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords.
       */
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
export type PublicErrorCode =
  | "bridge_protocol_incompatible"
  | "bridge_message_too_large"
  | "bridge_rate_limited"
  | "bridge_in_flight_limit"
  | "bridge_subscription_limit"
  | "bridge_deadline"
  | "bridge_cancelled"
  | "page_binding_missing"
  | "page_generation_stale"
  | "package_digest_stale"
  | "grant_revoked"
  | "grant_scope_denied"
  | "contract_not_found"
  | "contract_incompatible"
  | "schema_invalid"
  | "revision_conflict"
  | "cursor_gap"
  | "resource_handle_invalid"
  | "resource_handle_expired"
  | "resource_handle_exhausted"
  | "operation_pending"
  | "provider_unavailable"
  | "plugin_ui_isolation_unavailable";
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
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      /**
       * Inclusive range: 1..=30000.
       */
      deadlineMs: number;
      generation: number;
      id: string;
      method: string;
      namespace: string;
      params: unknown;
      type: "request";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      cursor: number;
      generation: number;
      type: "poll";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      generation: number;
      id: string;
      type: "cancel";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      generation: number;
      subscriptionId: string;
      type: "unsubscribe";
      /**
       * Must equal 1.
       */
      v: 1;
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
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      generation: number;
      id: string;
      result: unknown;
      type: "response";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      cursor: number;
      generation: number;
      id: string;
      subscriptionId: string;
      type: "subscribeResult";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      cursor: number;
      event: unknown;
      generation: number;
      subscriptionId: string;
      type: "event";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      earliestCursor: number;
      generation: number;
      latestCursor: number;
      requestedCursor: number;
      subscriptionId: string;
      type: "gap";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      code: PublicErrorCode;
      generation: number;
      type: "close";
      /**
       * Must equal 1.
       */
      v: 1;
    }
  | {
      code: PublicErrorCode;
      correlationId?: string | null;
      generation: number;
      id?: string | null;
      type: "error";
      /**
       * Must equal 1.
       */
      v: 1;
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
export type PagePlacement =
  "sidebar" | "commandPalette" | "deepLink" | "pluginSettings";
export type SettingRecord =
  | {
      key: string;
      revision: number;
      scope: "user";
      value: SettingValue;
    }
  | {
      key: string;
      projectId: string;
      revision: number;
      scope: "project";
      value: SettingValue;
    };
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
      /**
       * UTF-8 byte length: 0..=65536. Validators must enforce x-maxUtf8Bytes; standard maxLength counts Unicode scalars.
       */
      value: string;
    }
  | {
      reference: CredentialReference;
      type: "credentialReference";
    };
export type SettingWrite =
  | {
      expectedRevision: number;
      key: string;
      scope: "user";
      value: SettingValue;
    }
  | {
      expectedRevision: number;
      key: string;
      projectId: string;
      scope: "project";
      value: SettingValue;
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
  /**
   * Serialized JSON size must not exceed 262144 bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords.
   */
  data: unknown;
  id: string;
  revision: number;
  stale: boolean;
  state: string;
  updatedAtMs: number;
}
export interface EntityQuery {
  /**
   * Inclusive range: 1..=128.
   */
  limit: number;
  projection?: FieldProjection | null;
  /**
   * @minItems 1
   * @maxItems 128
   */
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
  /**
   * @maxItems 128
   */
  entities: EntityEnvelope[];
  snapshotRevision: number;
}
export interface EntityWatchRequest {
  cursor: number;
  /**
   * Inclusive range: 1..=128.
   */
  limit: number;
  projection?: FieldProjection | null;
  /**
   * @minItems 1
   * @maxItems 128
   */
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
  /**
   * Serialized JSON size must not exceed 131072 bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords.
   */
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
  /**
   * Serialized JSON size must not exceed 131072 bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords.
   */
  data: unknown;
  eventId: string;
  kind: string;
  streamId: string;
  subject: string;
}
export interface EventWatchRequest {
  contract: ContractRef;
  cursor: number;
  /**
   * Inclusive range: 1..=128.
   */
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
  /**
   * @minItems 1
   * @maxItems 128
   */
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
  code: PublicErrorCode;
}
export interface RuntimeOperationGap {
  earliestCursor: number;
  latestCursor: number;
  requestedCursor: number;
}
export interface RuntimeOperationQuery {
  includeTerminalSince?: number | null;
  /**
   * Inclusive range: 1..=128.
   */
  limit: number;
  /**
   * @minItems 1
   * @maxItems 128
   */
  subjects: OperationSubjectRef[];
}
export interface RuntimeOperationWatch {
  cursor: number;
  /**
   * Inclusive range: 1..=128.
   */
  limit: number;
  /**
   * @minItems 1
   * @maxItems 128
   */
  subjects: OperationSubjectRef[];
}
export interface TypedCommandDeclaration {
  command: ContractRef;
  riskFloor: Risk;
}
export interface TypedCommandInvocation {
  /**
   * Serialized JSON size must not exceed 262144 bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords.
   */
  args: unknown;
  command: ContractRef;
  /**
   * Inclusive range: 1..=30000.
   */
  deadlineMs: number;
  subject: OperationSubjectRef;
}
export interface BridgeContractV1 {
  clientFrame: BridgeClientFrame;
  hostFrame: BridgeHostFrame;
}
export interface ResolvedContributions {
  /**
   * @maxItems 512
   */
  actions?: ResolvedActionContribution[];
  /**
   * @maxItems 512
   */
  commands?: ResolvedCommandContribution[];
  /**
   * @maxItems 512
   */
  hotkeys?: ResolvedHotkeyContribution[];
  /**
   * @maxItems 512
   */
  pages?: ResolvedPageContribution[];
}
export interface ResolvedActionContribution {
  command: string;
  /**
   * @maxItems 16
   */
  context?: ContextReference[];
  id: string;
  /**
   * @minItems 1
   * @maxItems 16
   */
  locations: ActionLocation[];
  riskFloor: Risk;
  /**
   * UTF-8 byte length: 1..=256. Validators must enforce x-maxUtf8Bytes; standard maxLength counts Unicode scalars.
   */
  title: string;
}
export interface ResolvedCommandContribution {
  /**
   * @maxItems 16
   */
  context?: ContextReference[];
  id: string;
  /**
   * @minItems 1
   * @maxItems 16
   */
  placements: CommandPlacement[];
  riskFloor: Risk;
  /**
   * UTF-8 byte length: 1..=256. Validators must enforce x-maxUtf8Bytes; standard maxLength counts Unicode scalars.
   */
  title: string;
}
export interface ResolvedHotkeyContribution {
  command: string;
  scope: HotkeyScope;
  /**
   * UTF-8 byte length: 1..=128. Validators must enforce x-maxUtf8Bytes; standard maxLength counts Unicode scalars.
   */
  shortcut: string;
}
export interface ResolvedPageContribution {
  id: string;
  instancePolicy: InstancePolicy;
  /**
   * @minItems 1
   * @maxItems 16
   */
  placements: PagePlacement[];
  /**
   * UTF-8 byte length: 1..=256. Validators must enforce x-maxUtf8Bytes; standard maxLength counts Unicode scalars.
   */
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
export interface CredentialReference {
  credentialId: string;
}
