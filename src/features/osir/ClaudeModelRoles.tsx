import { useEffect, useId, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Provider } from "@/types";
import { providersApi } from "@/lib/api/providers";
import { fetchModelsForConfig } from "@/lib/api/model-fetch";
import { toast } from "@/lib/toast";
import {
  isCodexConversationModel,
  readConnection,
  simpleEditable,
} from "./providerConfig";
import "./claude-model-roles.css";

const ROLES = [
  { field: "ANTHROPIC_MODEL", label: "默认模型" },
  { field: "ANTHROPIC_DEFAULT_HAIKU_MODEL", label: "快速模型（如支持）" },
  { field: "ANTHROPIC_DEFAULT_SONNET_MODEL", label: "主要模型" },
  { field: "ANTHROPIC_DEFAULT_OPUS_MODEL", label: "高级模型" },
] as const;
type RoleField = (typeof ROLES)[number]["field"];
export type ClaudeModelRoleValues = Record<RoleField, string>;

/** Role-only payload. Native must recheck the saved binding and roles, validate
 * against its catalog, and apply through the provider service. Empty strings
 * describe unspecified roles, not override removal. Credentials and unrelated
 * settings remain native-owned. */
export interface ClaudeModelRoleUpdate {
  providerId: string;
  expectedReference: unknown;
  expectedRoles: ClaudeModelRoleValues;
  roles: ClaudeModelRoleValues;
}
export interface ClaudeModelRolesProps {
  provider: Provider;
  disabled?: boolean;
  preview?: boolean;
  onSaveManaged?: (
    request: ClaudeModelRoleUpdate,
  ) => Promise<ClaudeModelRoleValues>;
  onSaved?: () => void | Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
  onBusyChange?: (busy: boolean) => void;
}

export function saveManagedClaudeModelRoles(request: ClaudeModelRoleUpdate) {
  return invoke<ClaudeModelRoleValues>("osir_update_claude_model_roles", {
    request,
  });
}

function readRoles(provider: Provider): ClaudeModelRoleValues {
  const env = provider.settingsConfig.env;
  const value = (field: string): string =>
    typeof env?.[field] === "string" ? env[field] : "";
  return {
    ANTHROPIC_MODEL: value("ANTHROPIC_MODEL"),
    ANTHROPIC_DEFAULT_HAIKU_MODEL:
      typeof env?.ANTHROPIC_DEFAULT_HAIKU_MODEL === "string"
        ? env.ANTHROPIC_DEFAULT_HAIKU_MODEL
        : value("ANTHROPIC_SMALL_FAST_MODEL"),
    ANTHROPIC_DEFAULT_SONNET_MODEL: value("ANTHROPIC_DEFAULT_SONNET_MODEL"),
    ANTHROPIC_DEFAULT_OPUS_MODEL: value("ANTHROPIC_DEFAULT_OPUS_MODEL"),
  };
}
const sameRoles = (a: ClaudeModelRoleValues, b: ClaudeModelRoleValues) =>
  ROLES.every(({ field }) => a[field] === b[field]);

export function ClaudeModelRoles(props: ClaudeModelRolesProps) {
  // Only role metadata identifies drafts. Never serialize the full provider or
  // inspect a managed connection's credential fields, even for React keys.
  const signature = JSON.stringify([
    props.provider.id,
    props.provider.meta?.osirBinding,
    props.provider.meta?.stackModels,
    props.provider.category,
    props.provider.meta?.providerType,
    props.provider.meta?.authBinding,
    readRoles(props.provider),
    props.preview,
  ]);
  // Manual credentials are already editable renderer data. Compare in memory
  // without serializing them into keys; managed rows never enter this branch.
  const connection =
    !props.provider.meta?.osirBinding &&
    simpleEditable(props.provider, "claude")
      ? readConnection(props.provider, "claude")
      : null;
  const endpoint = JSON.stringify([
    connection?.baseUrl,
    connection?.protocol,
    props.provider.meta?.customUserAgent,
  ]);
  const identity = useRef({
    signature,
    endpoint,
    apiKey: connection?.apiKey,
    generation: 0,
  });
  if (
    identity.current.signature !== signature ||
    identity.current.endpoint !== endpoint ||
    identity.current.apiKey !== connection?.apiKey
  ) {
    identity.current = {
      signature,
      endpoint,
      apiKey: connection?.apiKey,
      generation: identity.current.generation + 1,
    };
  }
  return <ClaudeModelRolesDraft key={identity.current.generation} {...props} />;
}

function ClaudeModelRolesDraft({
  provider,
  disabled = false,
  preview = false,
  onSaveManaged = saveManagedClaudeModelRoles,
  onSaved,
  onDirtyChange,
  onBusyChange,
}: ClaudeModelRolesProps) {
  const id = useId();
  const managed = !!provider.meta?.osirBinding;
  const editable = managed || simpleEditable(provider, "claude");
  const [saved, setSaved] = useState(() => readRoles(provider));
  const [draft, setDraft] = useState(saved);
  const isConversation = (model: string, metadata?: unknown) =>
    isCodexConversationModel(model, metadata) &&
    isCodexConversationModel(
      model,
      provider.settingsConfig.modelCatalog?.models?.find(
        (entry: { model: string }) => entry.model === model,
      ),
    );
  const [catalog, setCatalog] = useState(() =>
    [
      ...new Set([
        ...Object.values(saved).filter(Boolean),
        ...(provider.meta?.stackModels || [])
          .map((model) => model.model)
          .filter(Boolean),
      ]),
    ].filter((model) => isConversation(model)),
  );
  const [verified, setVerified] = useState<string[] | null>(null);
  const [imageOutputIds, setImageOutputIds] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [catalogError, setCatalogError] = useState("");
  const [saveError, setSaveError] = useState("");
  const operation = useRef(false);
  const mounted = useRef(true);
  const callbacks = useRef({ onDirtyChange, onBusyChange });
  callbacks.current = { onDirtyChange, onBusyChange };
  const dirty = !sameRoles(draft, saved);
  const busy = loading || saving;
  const isImageOutput = (model: string) =>
    !isConversation(model) || imageOutputIds.includes(model);
  const unavailable = (model: string) =>
    isImageOutput(model) || (verified !== null && !verified.includes(model));
  const imageOutputSelected = Object.values(draft).some(
    (model) => model && isImageOutput(model),
  );
  const invalid = Object.values(draft).some(
    (model) => model && unavailable(model),
  );
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    callbacks.current.onDirtyChange?.(dirty);
    return () => callbacks.current.onDirtyChange?.(false);
  }, [dirty]);
  useEffect(() => {
    callbacks.current.onBusyChange?.(saving);
    return () => callbacks.current.onBusyChange?.(false);
  }, [saving]);

  async function fetchCatalog() {
    if (operation.current || disabled || preview || !editable) return;
    operation.current = true;
    setLoading(true);
    setCatalogError("");
    try {
      // Branch before readConnection: managed credentials stay in native.
      const models = managed
        ? await invoke<Array<{ id: string }>>("osir_provider_models", {
            tool: "claude",
            providerId: provider.id,
          })
        : await fetchManualCatalog();
      if (!mounted.current) return;
      const ids = [
        ...new Set(
          models
            .filter((model) => isConversation(model.id, model))
            .map((model) => model.id.trim())
            .filter(Boolean),
        ),
      ];
      setImageOutputIds(
        models
          .filter((model) => !isConversation(model.id, model))
          .map((model) => model.id.trim()),
      );
      setCatalog(ids);
      setVerified(ids);
      if (!ids.length)
        setCatalogError("服务未返回可用对话模型，请检查连接和分组权限后重试。");
    } catch {
      if (mounted.current)
        setCatalogError(
          "模型获取失败，保留最近成功目录和当前选择，请检查连接后重试。",
        );
    } finally {
      operation.current = false;
      if (mounted.current) setLoading(false);
    }
  }
  async function fetchManualCatalog() {
    const connection = readConnection(provider, "claude");
    return fetchModelsForConfig(
      connection.baseUrl,
      connection.apiKey,
      false,
      undefined,
      provider.meta?.customUserAgent,
      {
        apiFormat:
          connection.protocol === "anthropic"
            ? "anthropic-messages"
            : connection.protocol,
      },
    );
  }
  async function save() {
    if (
      operation.current ||
      disabled ||
      preview ||
      !editable ||
      !dirty ||
      invalid
    )
      return;
    operation.current = true;
    setSaving(true);
    setSaveError("");
    try {
      let confirmedRoles = { ...draft };
      if (managed) {
        confirmedRoles = await onSaveManaged({
          providerId: provider.id,
          expectedReference: provider.meta!.osirBinding!.reference,
          expectedRoles: { ...saved },
          roles: { ...draft },
        });
        if (
          !confirmedRoles ||
          ROLES.some(
            ({ field }) =>
              typeof confirmedRoles[field] !== "string" ||
              (draft[field] && confirmedRoles[field] !== draft[field]),
          )
        )
          throw new Error("保存未确认");
      } else {
        // Preserve unrelated recent changes. A row that became managed must
        // never fall through to generic update. Native still rechecks ownership.
        const latest = (await providersApi.getAll("claude"))[provider.id];
        if (!mounted.current) return;
        if (
          !latest ||
          latest.meta?.osirBinding ||
          !simpleEditable(latest, "claude") ||
          !sameRoles(readRoles(latest), saved)
        )
          throw new Error("连接已改变");
        const originalConnection = readConnection(provider, "claude");
        const latestConnection = readConnection(latest, "claude");
        if (
          originalConnection.baseUrl !== latestConnection.baseUrl ||
          originalConnection.apiKey !== latestConnection.apiKey ||
          originalConnection.protocol !== latestConnection.protocol ||
          provider.meta?.customUserAgent !== latest.meta?.customUserAgent
        )
          throw new Error("连接已改变");
        const env = { ...latest.settingsConfig.env };
        for (const { field } of ROLES) {
          if (draft[field]) env[field] = draft[field];
          else delete env[field];
          if (field !== "ANTHROPIC_MODEL" && draft[field] !== saved[field])
            delete env[field + "_NAME"];
        }
        delete env.ANTHROPIC_SMALL_FAST_MODEL;
        // Match the existing native normalization, rather than promising that
        // deleting a role override selects a Claude Code default.
        const fallback =
          env.ANTHROPIC_MODEL || env.ANTHROPIC_DEFAULT_HAIKU_MODEL;
        for (const { field } of ROLES.slice(1)) {
          if (!(field in env) && fallback) env[field] = fallback;
        }
        const confirmed = await providersApi.update(
          { ...latest, settingsConfig: { ...latest.settingsConfig, env } },
          "claude",
        );
        if (!confirmed) throw new Error("保存未确认");
        confirmedRoles = readRoles({
          ...latest,
          settingsConfig: { ...latest.settingsConfig, env },
        });
      }
      if (!mounted.current) return;
      setSaved({ ...confirmedRoles });
      setDraft({ ...confirmedRoles });
      toast.success("模型角色已保存，请重启 Claude Code 使配置生效。");
      try {
        await onSaved?.();
      } catch {
        if (mounted.current)
          setSaveError(
            "模型角色已保存，但状态刷新失败，请刷新连接核对，勿重复提交。",
          );
      }
    } catch {
      if (mounted.current)
        setSaveError(
          "模型角色未确认保存，保留本次修改。请刷新连接核对后重试。",
        );
    } finally {
      operation.current = false;
      if (mounted.current) setSaving(false);
    }
  }
  return (
    <section
      className="osir-claude-model-roles"
      aria-label={provider.name + " 的模型角色"}
    >
      <div className="osir-claude-model-roles-heading">
        <strong>模型配置</strong>
        <button
          type="button"
          disabled={disabled || preview || busy || !editable}
          onClick={() => void fetchCatalog()}
        >
          {loading ? "获取中…" : catalogError ? "重试获取模型" : "获取模型"}
        </button>
      </div>
      <div className="osir-claude-model-roles-fields">
        {ROLES.map(({ field, label }) => {
          const options = [
            ...new Set(
              [
                ...catalog.filter((model) => !isImageOutput(model)),
                draft[field],
              ].filter(Boolean),
            ),
          ];
          const isDefault =
            !!draft[field] && draft[field] === draft.ANTHROPIC_MODEL;
          return (
            <div className="osir-claude-model-role-row" key={field}>
              <label htmlFor={id + "-" + field}>{label}</label>
              <select
                id={id + "-" + field}
                aria-describedby={id + "-hint"}
                aria-invalid={
                  draft[field] && unavailable(draft[field]) ? true : undefined
                }
                value={draft[field]}
                disabled={disabled || preview || busy || !editable}
                onChange={(event) => {
                  const value = event.currentTarget.value;
                  setDraft((current) => ({ ...current, [field]: value }));
                }}
              >
                <option value="" disabled>
                  未指定（沿用现有默认配置）
                </option>
                {options.map((model) => (
                  <option
                    key={model}
                    value={model}
                    disabled={unavailable(model)}
                  >
                    {model}
                    {isImageOutput(model)
                      ? "（不可用于对话，请替换）"
                      : unavailable(model)
                        ? "（已不可用）"
                        : ""}
                  </option>
                ))}
              </select>
              <div className="osir-model-row-actions">
                <button
                  type="button"
                  className="osir-model-default"
                  aria-label={`${label} ${draft[field] || "未指定"} ${isDefault ? "已是默认模型" : "设为默认模型"}`}
                  aria-pressed={isDefault}
                  disabled={
                    disabled ||
                    preview ||
                    busy ||
                    !editable ||
                    !draft[field] ||
                    unavailable(draft[field]) ||
                    isDefault
                  }
                  onClick={() => {
                    setDraft((current) => ({
                      ...current,
                      ANTHROPIC_MODEL: current[field],
                    }));
                    setSaveError("");
                  }}
                >
                  {isDefault ? "默认模型" : "设为默认"}
                </button>
              </div>
            </div>
          );
        })}
      </div>
      <p id={id + "-hint"}>
        模型名称以当前连接可用列表为准；未指定的角色沿用现有默认配置。
      </p>
      {!catalog.length && <p role="status">尚无模型目录，请先获取可用模型。</p>}
      {preview ? (
        <p>浏览器预览不能获取模型或保存本机配置。</p>
      ) : !editable ? (
        <p>此连接使用工具原生配置，请在 Claude Code 中调整模型。</p>
      ) : null}
      {imageOutputSelected ? (
        <p role="alert">
          已保存的图片输出模型不可用于对话，请替换对应角色后保存。
        </p>
      ) : invalid ? (
        <p role="alert">所选模型已不在可用目录中，请重新选择。</p>
      ) : null}
      {catalogError && <p role="alert">{catalogError}</p>}
      {saveError && <p role="alert">{saveError}</p>}
      {dirty && (
        <div className="osir-claude-model-roles-actions osir-model-savebar">
          <span role="status">模型角色尚未保存</span>
          <button
            type="button"
            disabled={disabled || busy}
            onClick={() => {
              setDraft({ ...saved });
              setSaveError("");
            }}
          >
            撤销修改
          </button>
          <button
            type="button"
            className="primary"
            disabled={
              disabled || preview || busy || !editable || !dirty || invalid
            }
            onClick={() => void save()}
          >
            {saving ? "正在保存…" : "保存模型角色"}
          </button>
        </div>
      )}
    </section>
  );
}
