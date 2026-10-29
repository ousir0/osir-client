import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Cloud,
  ExternalLink,
  Monitor,
  Plus,
  RefreshCw,
  Settings2,
} from "lucide-react";
import { providersApi, settingsApi } from "@/lib/api";
import type { Provider } from "@/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { SessionManagerPage } from "@/components/sessions/SessionManagerPage";
import { toast } from "@/lib/toast";
import { ConnectionDrawer } from "./ConnectionDrawer";
import { OsirTabs } from "./OsirTabs";
import { SupportedToolIcon } from "./SupportedToolIcon";
import { ToolLaunchButton } from "./ToolLaunchButton";
import { ToolCloseButton } from "./ToolCloseButton";
import { useLocalTools } from "./toolsApi";
import { connectionError } from "./errors";
import { modelNames } from "./providerConfig";
import "./claude-desktop.css";

const TOOL = "claude-desktop" as const;
type ConnectionMode = {
  mode: "native" | "osir_api" | "unavailable";
  canRestoreNative: boolean;
  reason?: string | null;
};

/** Desktop has its own provider/profile contract; never use the CLI's live files. */
export function OsirClaudeDesktop({
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
  const mode = useQuery({
    queryKey: ["osir-connection-mode", TOOL],
    queryFn: () =>
      invoke<ConnectionMode>("osir_get_connection_mode", { tool: TOOL }),
    enabled: desktop,
    retry: false,
    refetchOnWindowFocus: false,
  });
  const status = useQuery({
    queryKey: ["osir-claude-desktop-status"],
    queryFn: () => providersApi.getClaudeDesktopStatus(),
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
    onNavigationBlockedChange(busy || !!drawer || !!deletion);
    return () => onNavigationBlockedChange(false);
  }, [busy, drawer, deletion, onNavigationBlockedChange]);
  const rows = Object.values(providers.data ?? {}).filter(
    (provider) => provider.category !== "official",
  );
  const activeProvider = rows.find(
    (provider) => provider.id === status.data?.activeProviderId,
  );
  const appliedConnection =
    !!activeProvider &&
    !status.isError &&
    status.data?.configured &&
    !status.data.missingRouteMappings &&
    !status.data.staleRawModels &&
    !!status.data.expectedBaseUrl &&
    status.data.actualBaseUrl === status.data.expectedBaseUrl &&
    (!activeProvider.meta?.osirBinding && status.data.mode === "direct"
      ? true
      : status.data.mode !== "direct" &&
        status.data.proxyRunning &&
        status.data.gatewayTokenConfigured);
  const actualMode =
    !mode.isError &&
    !mode.isPending &&
    !(mode.data?.mode === "native" && status.data?.activeProviderId)
      ? mode.data?.mode
      : undefined;

  function refresh() {
    for (const key of [
      ["osir-providers", TOOL],
      ["osir-connection-mode", TOOL],
      ["osir-claude-desktop-status"],
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
      return true;
    } catch (error) {
      if (mounted.current) setFailure(connectionError(error));
      refresh();
    } finally {
      mutation.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  const useProvider = (provider: Provider) =>
    apply(async () => {
      if (provider.meta?.osirBinding) {
        await invoke("osir_set_connection_mode", {
          tool: TOOL,
          mode: "osir_api",
          providerId: provider.id,
        });
      } else {
        await providersApi.switch(provider.id, TOOL);
      }
    }, "Claude 桌面连接已应用。请保存工作后重启，并新建对话验证。");

  async function download() {
    try {
      await settingsApi.openExternal("https://claude.ai/download");
    } catch {
      setFailure("无法打开官方下载页面，请稍后重试。");
    }
  }

  return (
    <section className="osir-claude-desktop" aria-label="Claude 桌面客户端配置">
      <header className="osir-header">
        <div className="osir-claude-desktop-heading">
          <SupportedToolIcon id={TOOL} size={26} />
          <div>
            <h1>Claude 桌面客户端</h1>
            <p>独立管理桌面连接与模型</p>
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
        label="Claude 桌面客户端功能"
        value={tab}
        onChange={(value) => {
          if (!busy && !drawer && !deletion) setTab(value);
        }}
        items={[
          { value: "models", label: "模型接入" },
          { value: "history", label: "历史会话" },
          { value: "install", label: "安装与更新" },
        ]}
      />
      {!desktop && <p role="status">请在桌面客户端中配置 Claude 桌面连接。</p>}
      {failure && (
        <p role="alert" className="osir-error">
          {failure}
        </p>
      )}
      {tab === "models" ? (
        <>
          <div className="osir-section-title">
            <h2>选择连接方式</h2>
          </div>
          <div className="osir-connection-cards">
            <button
              className={actualMode === "native" ? "highlight" : ""}
              aria-pressed={actualMode === "native"}
              disabled={!desktop || busy || mode.isPending || mode.isError}
              onClick={() =>
                void apply(
                  () =>
                    invoke("osir_set_connection_mode", {
                      tool: TOOL,
                      mode: "native",
                    }),
                  "已恢复 Claude 桌面官方配置。请保存工作后重启。",
                )
              }
            >
              <span className="osir-small-icon">
                <Monitor size={22} />
              </span>
              <div>
                <strong>原生配置</strong>
                <small>使用 Claude 官方账户</small>
              </div>
              {actualMode === "native" && <Check size={17} />}
            </button>
            <button
              className={actualMode === "osir_api" ? "highlight" : ""}
              aria-pressed={actualMode === "osir_api"}
              disabled={!desktop || busy || mode.isPending || mode.isError}
              onClick={() => {
                if (actualMode === "osir_api") return;
                const provider = rows.find((row) => !!row.meta?.osirBinding);
                if (provider) void useProvider(provider);
                else setDrawer({});
              }}
            >
              <span className="osir-small-icon">
                <Cloud size={22} />
              </span>
              <div>
                <strong>OSIR API</strong>
                <small>选择账户密钥与可用模型</small>
              </div>
              {actualMode === "osir_api" && <Check size={17} />}
            </button>
          </div>
          {(mode.isError || providers.isError || status.isError) && (
            <div className="osir-error" role="alert">
              连接状态读取失败，请重试后核对。
              <button onClick={refresh}>重新读取连接</button>
            </div>
          )}
          <div className="osir-section-title">
            <h2>
              已添加供应商 <span>{rows.length}</span>
            </h2>
            <div className="osir-actions">
              <button disabled={!desktop || busy} onClick={() => setDrawer({})}>
                <Plus size={16} /> 添加供应商
              </button>
              <button
                aria-label="刷新 Claude 桌面供应商"
                disabled={!desktop || busy || providers.isFetching}
                onClick={refresh}
              >
                <RefreshCw
                  size={16}
                  className={providers.isFetching ? "animate-spin" : undefined}
                />
              </button>
            </div>
          </div>
          {providers.isPending && desktop ? (
            <p role="status">正在读取桌面连接…</p>
          ) : !providers.isError && !rows.length ? (
            <div className="osir-claude-desktop-empty">
              <Cloud size={24} />
              <strong>添加你的第一条桌面连接</strong>
              <p>选择 OSIRAPI 账户密钥，或手动配置服务地址和模型。</p>
              <button disabled={!desktop || busy} onClick={() => setDrawer({})}>
                添加供应商
              </button>
            </div>
          ) : null}
          <div className="osir-provider-list">
            {rows.map((provider) => {
              const models = modelNames(provider, TOOL);
              const active =
                appliedConnection && activeProvider.id === provider.id;
              const open = expanded[provider.id] === true;
              return (
                <article
                  className="osir-claude-desktop-provider"
                  key={provider.id}
                  aria-label={`${provider.name} 桌面连接`}
                  onClick={(event) => {
                    const target = event.target as HTMLElement;
                    if (
                      !target.closest(
                        ".osir-claude-desktop-models, button, input, select, a",
                      )
                    ) {
                      setExpanded((state) => ({
                        ...state,
                        [provider.id]: false,
                      }));
                    }
                  }}
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
                          !desktop || busy || status.isPending || status.isError
                        }
                        onClick={() => void useProvider(provider)}
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
                      disabled={busy}
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
                    <div
                      className="osir-claude-desktop-models"
                      aria-label={`${provider.name} 的桌面模型`}
                    >
                      {models.map((model) => (
                        <span key={model}>{model}</span>
                      ))}
                    </div>
                  )}
                </article>
              );
            })}
          </div>
          {status.data?.configured && (
            <p className="osir-muted" role="status">
              {appliedConnection
                ? "连接已保存。请重启 Claude，并在新对话中选择模型。"
                : "此连接尚未生效，请重新选择并应用。"}
            </p>
          )}
          <p className="osir-muted osir-claude-desktop-scope">
            可在“历史会话”查看本机保存的 Code/Cowork 记录；普通云端聊天请在
            Claude 应用中查看。
          </p>
        </>
      ) : tab === "history" ? (
        <div className="osir-claude-desktop-history">
          <SessionManagerPage
            appId={TOOL}
            fromApp={TOOL}
            scopedToApp
            osirVariant
          />
        </div>
      ) : (
        <div className="osir-claude-desktop-install">
          <div className="osir-section-title">
            <h2>Claude 桌面客户端</h2>
            <button
              disabled={!desktop || tools.isFetching}
              onClick={() => void tools.refetch()}
            >
              <RefreshCw size={15} />
              重新检测
            </button>
          </div>
          <p>
            {installation?.installed
              ? `当前版本 ${installation.version ?? "暂不可用"}`
              : tools.isError
                ? "安装状态读取失败"
                : "尚未检测到安装"}
          </p>
          <p className="osir-muted">
            安装使用官方下载，版本更新由 Claude 应用内检查。
          </p>
          <button disabled={!desktop} onClick={() => void download()}>
            <ExternalLink size={15} />
            官方下载
          </button>
        </div>
      )}
      <div className="osir-claude-desktop-footer">
        <ToolCloseButton
          tool={TOOL}
          installation={installation}
          desktop={desktop}
          disabled={busy}
        />
      </div>
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
          onSaved={(provider) => {
            setExpanded((state) => ({ ...state, [provider.id]: true }));
            refresh();
            toast.success(
              "Claude 桌面连接已保存，请在供应商卡片切换使用后重启应用。",
            );
          }}
        />
      )}
      <ConfirmDialog
        isOpen={!!deletion}
        title="删除桌面连接"
        message={`确认删除「${deletion?.name ?? ""}」？仅删除本地连接，云端密钥保持不变。`}
        variant="destructive"
        pending={busy}
        onCancel={() => {
          if (!busy) setDeletion(null);
        }}
        onConfirm={() => {
          if (!deletion) return;
          void apply(async () => {
            const confirmed = await providersApi.delete(deletion.id, TOOL);
            if (!confirmed) throw new Error("删除未确认完成，请刷新后核对");
            setDeletion(null);
            setDrawer(null);
          }, "本地桌面连接已删除。");
        }}
      />
    </section>
  );
}
