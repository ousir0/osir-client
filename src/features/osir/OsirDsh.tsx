import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Plus,
  RefreshCw,
  Settings2,
} from "lucide-react";
import { providersApi } from "@/lib/api";
import type { Provider } from "@/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { SessionManagerPage } from "@/components/sessions/SessionManagerPage";
import { toast } from "@/lib/toast";
import { ConnectionDrawer } from "./ConnectionDrawer";
import { OsirTabs } from "./OsirTabs";
import { SupportedToolIcon } from "./SupportedToolIcon";
import { ToolLaunchButton } from "./ToolLaunchButton";
import { useLocalTools } from "./toolsApi";
import { connectionError } from "./errors";
import { modelNames, readConnection } from "./providerConfig";
import { ToolConfigBackups } from "./ToolConfigBackups";
import { OsirToolsHub } from "./OsirToolsHub";
import "./claude-desktop.css";
const TOOL = "dsh" as const;

export function OsirDsh({
  desktop,
  onNavigationBlockedChange,
  onConnectAccount,
}: {
  desktop: boolean;
  onNavigationBlockedChange: (blocked: boolean) => void;
  onConnectAccount: () => void;
}) {
  const query = useQueryClient();
  const tools = useLocalTools(desktop);
  const installation = tools.data?.find((item) => item.id === TOOL);
  const [tab, setTab] = useState("models");
  const [drawer, setDrawer] = useState<{ provider?: Provider } | null>(null);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);
  const [backupBusy, setBackupBusy] = useState(false);
  const mutation = useRef(false);
  const mounted = useRef(true);
  const [failure, setFailure] = useState("");
  const [deletion, setDeletion] = useState<Provider | null>(null);
  const providers = useQuery({
    queryKey: ["osir-providers", TOOL],
    queryFn: () => providersApi.getAll(TOOL),
    enabled: desktop,
    retry: false,
  });
  const current = useQuery({
    queryKey: ["osir-current-provider", TOOL],
    queryFn: () => providersApi.getCurrent(TOOL),
    enabled: desktop,
    retry: false,
  });
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    onNavigationBlockedChange(busy || backupBusy || !!drawer || !!deletion);
    return () => onNavigationBlockedChange(false);
  }, [busy, backupBusy, drawer, deletion, onNavigationBlockedChange]);
  const rows = Object.values(providers.data ?? {}).filter(
    (p) => p.category !== "official",
  );
  function refresh() {
    for (const key of [
      ["osir-providers", TOOL],
      ["osir-current-provider", TOOL],
      ["providers", TOOL],
    ])
      void query.invalidateQueries({ queryKey: key });
  }
  async function apply(operation: () => Promise<unknown>, success: string) {
    if (mutation.current || !desktop) return;
    mutation.current = true;
    setBusy(true);
    setFailure("");
    try {
      await operation();
      refresh();
      toast.success(success);
    } catch (error) {
      if (mounted.current) setFailure(connectionError(error));
      refresh();
    } finally {
      mutation.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  return (
    <section className="osir-claude-desktop" aria-label="DeepSeek Harness 配置">
      <header className="osir-header">
        <div className="osir-claude-desktop-heading">
          <SupportedToolIcon id={TOOL} size={26} />
          <div>
            <h1>DeepSeek Harness</h1>
            <p>管理供应商、默认模型与本机会话</p>
          </div>
        </div>
        <div className="osir-actions">
          <ToolLaunchButton
            tool={TOOL}
            installation={installation}
            desktop={desktop}
            disabled={busy}
          />
        </div>
      </header>
      <OsirTabs
        label="DeepSeek Harness 功能"
        value={tab}
        onChange={(value) => {
          if (!busy && !backupBusy && !drawer && !deletion) setTab(value);
        }}
        items={[
          { value: "models", label: "模型接入" },
          { value: "history", label: "历史会话" },
          { value: "backup", label: "配置备份" },
          { value: "install", label: "安装与更新" },
        ]}
      />
      {failure && (
        <p role="alert" className="osir-error">
          {failure}
        </p>
      )}
      {tab === "models" ? (
        <>
          <div className="osir-section-title">
            <h2>
              已添加供应商 <span>{rows.length}</span>
            </h2>
            <div className="osir-actions">
              <button disabled={!desktop || busy} onClick={() => setDrawer({})}>
                <Plus size={16} /> 添加供应商
              </button>
              <button
                aria-label="刷新 DeepSeek 供应商"
                disabled={!desktop || busy || providers.isFetching}
                onClick={refresh}
              >
                <RefreshCw size={16} />
              </button>
            </div>
          </div>
          {!desktop && <p role="status">请在桌面客户端中配置连接。</p>}
          {(providers.isError || current.isError) && (
            <div role="alert" className="osir-error">
              连接读取失败，请检查 DeepSeek 配置后重试。
              <button onClick={refresh}>重新读取</button>
            </div>
          )}
          {providers.isPending && desktop ? (
            <p role="status">正在读取连接…</p>
          ) : !providers.isError && !rows.length ? (
            <div className="osir-claude-desktop-empty">
              <strong>添加你的第一条连接</strong>
              <p>选择 OSIRAPI 账户密钥，或填写服务地址、密钥和对话模型。</p>
              <button disabled={!desktop} onClick={() => setDrawer({})}>
                添加供应商
              </button>
            </div>
          ) : null}
          <div className="osir-provider-list">
            {rows.map((provider) => {
              const models = modelNames(provider, TOOL);
              const active = !current.isError && current.data === provider.id;
              const open = expanded[provider.id] === true;
              return (
                <article
                  className="osir-claude-desktop-provider"
                  key={provider.id}
                  aria-label={`${provider.name} 连接`}
                >
                  <div className="osir-claude-desktop-provider-heading">
                    <SupportedToolIcon id={TOOL} size={22} />
                    <div className="osir-claude-desktop-provider-name">
                      <strong>{provider.name}</strong>
                      <small>
                        {models.length} 个模型
                        {provider.meta?.osirBinding
                          ? ` · ${provider.meta.osirBinding.masked_key}`
                          : " · 手动连接"}
                      </small>
                    </div>
                    {active ? (
                      <span className="osir-claude-desktop-active">
                        <Check size={14} />
                        使用中
                      </span>
                    ) : (
                      <button
                        disabled={
                          !desktop ||
                          busy ||
                          current.isPending ||
                          current.isError
                        }
                        onClick={() =>
                          void apply(
                            () => providersApi.switch(provider.id, TOOL),
                            "连接与默认模型已应用。请在 DeepSeek Harness 中新建对话。",
                          )
                        }
                      >
                        切换使用
                      </button>
                    )}
                    <button
                      aria-label={`配置 ${provider.name}`}
                      disabled={busy}
                      onClick={() => setDrawer({ provider })}
                    >
                      <Settings2 size={16} />
                    </button>
                    <button
                      aria-label={`${open ? "收起" : "展开"} ${provider.name} 的模型`}
                      aria-expanded={open}
                      onClick={() =>
                        setExpanded((state) => ({
                          ...state,
                          [provider.id]: !open,
                        }))
                      }
                    >
                      {open ? (
                        <ChevronDown size={16} />
                      ) : (
                        <ChevronRight size={16} />
                      )}
                    </button>
                  </div>
                  {open && (
                    <div className="osir-claude-desktop-models">
                      {models.map((model) => (
                        <span key={model}>
                          {model}
                          {readConnection(provider, TOOL).model === model
                            ? " · 默认"
                            : ""}
                        </span>
                      ))}
                    </div>
                  )}
                </article>
              );
            })}
          </div>
          <p className="osir-muted">
            切换会应用到
            Web、桌面及已初始化的命令行配置；新对话使用所选默认模型。DeepSeek
            官方账户可在工具内继续使用。
          </p>
        </>
      ) : tab === "history" ? (
        <SessionManagerPage
          appId={TOOL}
          fromApp={TOOL}
          scopedToApp
          osirVariant
        />
      ) : tab === "backup" ? (
        <ToolConfigBackups
          tool={TOOL}
          disabled={!desktop || busy}
          onBlockedChange={setBackupBusy}
          onRestored={refresh}
        />
      ) : (
        <OsirToolsHub
          onOpenTool={() => setTab("models")}
          desktop={desktop}
          selectedTool={TOOL}
          onNavigationBlockedChange={setBackupBusy}
        />
      )}
      {drawer && (
        <ConnectionDrawer
          key={drawer.provider?.id ?? "new"}
          tool={TOOL}
          provider={drawer.provider}
          preview={!desktop}
          onClose={() => setDrawer(null)}
          onConnectAccount={() => {
            setDrawer(null);
            onConnectAccount();
          }}
          onDelete={
            drawer.provider ? () => setDeletion(drawer.provider!) : undefined
          }
          onSaved={() => {
            refresh();
            toast.success("连接已保存，请在供应商卡片切换使用。");
          }}
        />
      )}
      <ConfirmDialog
        isOpen={!!deletion}
        title="删除连接"
        message={`确认删除「${deletion?.name ?? ""}」？DeepSeek 中对应的供应商配置也会移除。`}
        variant="destructive"
        pending={busy}
        onCancel={() => {
          if (!busy) setDeletion(null);
        }}
        onConfirm={() => {
          if (deletion)
            void apply(async () => {
              const ok = await providersApi.delete(deletion.id, TOOL);
              if (!ok) throw new Error("删除未确认完成，请刷新后核对");
              setDeletion(null);
              setDrawer(null);
            }, "连接已删除。");
        }}
      />
    </section>
  );
}
