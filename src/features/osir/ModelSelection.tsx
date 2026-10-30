import { useEffect, useRef, useState } from "react";
import type { Provider } from "@/types";
import {
  isCodexConversationModel,
  modelNames,
  readConnection,
  setCodexModels,
} from "./providerConfig";
import { fetchModelsForConfig } from "@/lib/api/model-fetch";
import { invoke } from "@tauri-apps/api/core";
import { X } from "lucide-react";

type ModelSelectionProps = {
  provider: Provider;
  disabled: boolean;
  preview: boolean;
  onSave: (p: Provider) => Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
};

export function ModelSelection(props: ModelSelectionProps) {
  // Keep credentials out of React's public element keys while isolating drafts
  // whenever a route, credential, catalog or configuration changes.
  const signature = JSON.stringify([props.provider, props.preview]);
  const identity = useRef({ signature, generation: 0 });
  if (identity.current.signature !== signature) {
    identity.current = {
      signature,
      generation: identity.current.generation + 1,
    };
  }
  return <ModelSelectionDraft key={identity.current.generation} {...props} />;
}

function ModelSelectionDraft({
  provider,
  disabled,
  preview,
  onSave,
  onDirtyChange,
}: ModelSelectionProps) {
  const initial = modelNames(provider, "codex").filter((id) => {
    const metadata = provider.settingsConfig.modelCatalog?.models?.find(
      (model: { model: string }) => model.model === id,
    );
    return isCodexConversationModel(id, metadata);
  });
  const initialDefault = readConnection(provider, "codex").model;
  const [available, setAvailable] = useState(initial);
  const availableBeforeEdits = useRef(initial);
  const removed = useRef(new Set<string>());
  const saved = useRef({ selected: initial, defaultModel: initialDefault });
  const [selected, setSelected] = useState(initial);
  const [defaultModel, setDefaultModel] = useState(initialDefault);
  const [modelInput, setModelInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [dirty, setDirty] = useState(false);
  const [verified, setVerified] = useState<string[] | null>(null);
  const [saving, setSaving] = useState(false);
  const [search, setSearch] = useState("");
  const dirtyCallback = useRef(onDirtyChange);
  dirtyCallback.current = onDirtyChange;
  useEffect(() => {
    dirtyCallback.current?.(dirty);
    return () => dirtyCallback.current?.(false);
  }, [dirty]);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const busy = disabled || loading || saving;
  const invalid = selected.some(
    (model) => verified !== null && !verified.includes(model),
  );
  const validDefault = !!defaultModel && selected.includes(defaultModel);
  async function fetch() {
    setLoading(true);
    setError("");
    try {
      const connection = readConnection(provider, "codex");
      const models = provider.meta?.osirBinding
        ? await invoke<Array<{ id: string }>>("osir_provider_models", {
            tool: "codex",
            providerId: provider.id,
          })
        : await fetchModelsForConfig(
            connection.baseUrl,
            connection.apiKey,
            false,
            undefined,
            undefined,
            { apiFormat: connection.protocol },
          );
      if (!mounted.current) return;
      const ids = [
        ...new Set(
          models
            .filter(
              (m) =>
                isCodexConversationModel(m.id, m) &&
                isCodexConversationModel(
                  m.id,
                  provider.settingsConfig.modelCatalog?.models?.find(
                    (savedModel: { model: string }) =>
                      savedModel.model === m.id,
                  ),
                ),
            )
            .map((m) => m.id.trim())
            .filter(Boolean),
        ),
      ];
      setVerified(ids);
      setAvailable(
        [...new Set([...selected, ...ids])].filter(
          (id) => !removed.current.has(id),
        ),
      );
      availableBeforeEdits.current = [
        ...new Set([...saved.current.selected, ...ids]),
      ];
      if (!ids.length) setError("服务未返回模型，请检查密钥的分组权限。");
    } catch {
      if (mounted.current)
        setError(
          "模型获取失败，保留最近成功目录（可能已过期），请检查连接配置和分组权限后重试。",
        );
    } finally {
      if (mounted.current) setLoading(false);
    }
  }
  return (
    <div className="osir-model-selection">
      <div className="osir-row">
        <small>勾选发布到 Codex 的模型</small>
        <button disabled={preview || busy} onClick={() => void fetch()}>
          {loading ? "获取中…" : "获取模型"}
        </button>
      </div>
      {available.length > 6 && (
        <input
          type="search"
          aria-label="搜索模型"
          placeholder="搜索模型名称"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      )}
      {!available.length && (
        <p className="osir-muted">尚无模型目录，请先获取可用模型。</p>
      )}
      {available.length > 0 &&
        !available.some((model) =>
          model.toLowerCase().includes(search.toLowerCase()),
        ) && <p role="status">没有匹配的模型，请尝试其他名称。</p>}
      <div className="osir-model-grid">
        {available
          .filter((model) => model.toLowerCase().includes(search.toLowerCase()))
          .map((model) => {
            const isDefault = model === defaultModel;
            const isAutoReview =
              provider.settingsConfig.modelCatalog?.autoReviewModel === model;
            const cannotRemove = isDefault || isAutoReview;
            const removeTitle = isDefault
              ? "请先选择其他默认模型"
              : isAutoReview
                ? "此模型用于自动审批，请先在 Codex 配置中更改审批模型"
                : "从此连接移除模型";
            return (
              <div className="osir-model-row" key={model}>
                <label>
                  <input
                    type="checkbox"
                    checked={selected.includes(model)}
                    disabled={
                      busy ||
                      (model === defaultModel &&
                        (verified === null || verified.includes(model))) ||
                      (!selected.includes(model) &&
                        verified !== null &&
                        !verified.includes(model))
                    }
                    onChange={(e) => {
                      setSelected(
                        e.target.checked
                          ? [...selected, model]
                          : selected.filter((m) => m !== model),
                      );
                      if (!e.target.checked && model === defaultModel)
                        setDefaultModel("");
                      setDirty(true);
                    }}
                  />
                  {model}
                  {verified !== null && !verified.includes(model)
                    ? "（已不可用）"
                    : ""}
                </label>
                <div className="osir-model-row-actions">
                  <button
                    type="button"
                    className="osir-model-default"
                    disabled={
                      busy ||
                      !selected.includes(model) ||
                      (verified !== null && !verified.includes(model))
                    }
                    aria-pressed={model === defaultModel}
                    onClick={() => {
                      setDefaultModel(model);
                      setDirty(true);
                    }}
                  >
                    {model === defaultModel ? "默认模型" : "设为默认"}
                  </button>
                  <button
                    type="button"
                    className="osir-model-remove"
                    aria-label={`移除模型 ${model}`}
                    title={removeTitle}
                    disabled={busy || cannotRemove}
                    onClick={() => {
                      removed.current.add(model);
                      setAvailable((models) =>
                        models.filter((item) => item !== model),
                      );
                      setSelected((models) =>
                        models.filter((item) => item !== model),
                      );
                      setDirty(true);
                    }}
                  >
                    <X size={14} aria-hidden="true" />
                  </button>
                </div>
              </div>
            );
          })}
      </div>
      {!preview && verified === null && (
        <p className="osir-muted">
          新增模型前请先获取目录；获取失败可重试，现有选择不会丢失。
        </p>
      )}
      {!provider.meta?.osirBinding && (
        <div className="osir-row">
          <input
            aria-label="添加模型 ID"
            placeholder="填写模型 ID"
            value={modelInput}
            disabled={busy}
            onChange={(e) => setModelInput(e.target.value)}
          />
          <button
            disabled={
              busy ||
              !modelInput.trim() ||
              !isCodexConversationModel(modelInput.trim()) ||
              (!preview && !verified?.includes(modelInput.trim()))
            }
            onClick={() => {
              const m = modelInput.trim();
              removed.current.delete(m);
              setAvailable([...new Set([...available, m])]);
              setSelected([...new Set([...selected, m])]);
              if (!defaultModel) setDefaultModel(m);
              setModelInput("");
              setDirty(true);
            }}
          >
            添加模型
          </button>
        </div>
      )}
      {invalid && (
        <p role="alert">所选模型已不可用，请取消勾选并重新选择默认模型。</p>
      )}
      {!validDefault && <p role="alert">请选择一个已勾选的默认模型。</p>}
      {error && (
        <p role="alert" className="osir-error">
          {error}
        </p>
      )}
      {dirty && (
        <div className="osir-model-savebar">
          <span role="status">模型选择尚未保存</span>
          <button
            disabled={busy}
            onClick={() => {
              setSelected(saved.current.selected);
              setAvailable(availableBeforeEdits.current);
              setDefaultModel(saved.current.defaultModel);
              removed.current.clear();
              setModelInput("");
              setError("");
              setDirty(false);
            }}
          >
            撤销修改
          </button>
          <button
            className="primary"
            disabled={busy || invalid || !validDefault}
            onClick={async () => {
              setSaving(true);
              setError("");
              try {
                await onSave(
                  setCodexModels(
                    provider,
                    selected,
                    defaultModel,
                    verified ?? undefined,
                  ),
                );
                if (mounted.current) {
                  saved.current = { selected: [...selected], defaultModel };
                  availableBeforeEdits.current = [...available];
                  removed.current.clear();
                  setDirty(false);
                }
              } catch {
                if (mounted.current) setError("模型未保存，请检查状态后重试。");
              } finally {
                if (mounted.current) setSaving(false);
              }
            }}
          >
            {saving ? "正在保存…" : "保存模型选择"}
          </button>
        </div>
      )}
    </div>
  );
}
