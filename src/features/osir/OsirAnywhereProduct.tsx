import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import {
  CheckCircle2,
  ExternalLink,
  Globe,
  Info,
  Laptop,
  Loader2,
  RefreshCw,
  Smartphone,
} from "lucide-react";
import { settingsApi } from "@/lib/api";
import { OsirAnywhereProvider, useOsirAnywhere } from "./useOsirAnywhere";
import "./anywhere.css";

export const ANYWHERE_WEB_URL = "https://anywhere.osirclaw.com";

type CodexStatus = {
  configured: boolean | null;
  configRoot: string | null;
  connectionMode: "direct" | "proxy" | "unavailable";
  proxyRunning: boolean | null;
  cliStatus: "installed" | "not_installed" | "error" | "unavailable";
  cliVersion: string | null;
};

function AnywhereIcon({ large = false }: { large?: boolean }) {
  return (
    <span
      className={`osir-anywhere-icon${large ? " large" : ""}`}
      aria-hidden="true"
    >
      <Laptop size={large ? 76 : 26} strokeWidth={1.4} />
      <Smartphone size={large ? 33 : 13} strokeWidth={1.5} />
    </span>
  );
}
function Status() {
  const { installed, label, local } = useOsirAnywhere();
  return (
    <span
      className="osir-anywhere-status"
      data-installed={!!installed}
      role="status"
    >
      {installed && <CheckCircle2 size={15} aria-hidden="true" />}
      {label}
      {installed && local.data?.version
        ? ` · v${local.data.version.replace(/^v/, "")}`
        : ""}
    </span>
  );
}
function Actions({
  compact = false,
  beforeInstall,
}: {
  compact?: boolean;
  beforeInstall?: () => void;
}) {
  const state = useOsirAnywhere();
  const { installed, action, local, installation, label, desktop } = state;
  const phase = installation.data?.phase;
  if (!desktop)
    return (
      <button disabled title={state.description}>
        桌面客户端可用
      </button>
    );
  if (installed)
    return (
      <button
        className="primary"
        disabled={!!action || local.isFetching}
        onClick={() => void state.launch()}
      >
        {action === "launch" ? (
          <Loader2 size={16} className="animate-spin" />
        ) : (
          <ExternalLink size={16} />
        )}
        {action === "launch" ? "正在打开" : compact ? "打开" : "打开 Anywhere"}
      </button>
    );
  if (
    phase === "downloading" &&
    !state.detectionFailed &&
    !installation.isError
  )
    return (
      <button disabled={!!action} onClick={() => void state.cancel()}>
        {action === "cancel" ? "正在取消" : "取消下载"}
      </button>
    );
  if (state.canInstall)
    return (
      <button
        className="primary"
        disabled={!!action}
        onClick={() => (beforeInstall ? beforeInstall() : void state.install())}
      >
        {action === "install" && <Loader2 size={15} className="animate-spin" />}
        {action === "install"
          ? "正在准备安装"
          : phase === "ready"
            ? "继续安装"
            : phase === "error" || phase === "cancelled"
              ? "重新安装"
              : "安装"}
      </button>
    );
  if (label === "即将开放" || label === "平台暂不支持")
    return (
      <button disabled title={state.description}>
        {label}
      </button>
    );
  return (
    <button
      disabled={
        !!action ||
        local.isFetching ||
        installation.isFetching ||
        label === "检测中"
      }
      onClick={() => void state.redetect()}
    >
      {action === "detect" ? (
        <Loader2 size={15} className="animate-spin" />
      ) : (
        <RefreshCw size={15} />
      )}
      {action === "detect" ? "正在检测" : "重新检测"}
    </button>
  );
}
export function anywhereBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
function DownloadProgress({ compact = false }: { compact?: boolean }) {
  const { installation, desktop } = useOsirAnywhere();
  const status = installation.data;
  if (!desktop || status?.phase !== "downloading" || installation.isError)
    return null;
  const total =
    status.totalBytes != null && status.totalBytes > 0
      ? status.totalBytes
      : null;
  const downloaded = Math.max(0, status.downloadedBytes);
  return (
    <div className={`osir-anywhere-progress${compact ? " compact" : ""}`}>
      {!compact && (
        <progress
          aria-label="Anywhere 下载进度"
          max={total ?? undefined}
          value={total ? Math.min(downloaded, total) : undefined}
        />
      )}
      <span>
        {anywhereBytes(downloaded)} /{" "}
        {total ? anywhereBytes(total) : "总大小未知"}
      </span>
    </div>
  );
}

function UnsignedInstallNotice() {
  const state = useOsirAnywhere();
  const release = state.release.data;
  if (
    !state.desktop ||
    state.installed ||
    state.detectionFailed ||
    state.local.data?.status !== "not_installed" ||
    state.release.isError ||
    release?.status !== "ready" ||
    release.systemSignature !== "unsigned"
  )
    return null;
  const message =
    release.platform === "macos" || release.platform === "darwin"
      ? "此安装包未签名。Mac 首次打开时，可能需要在“系统设置 → 隐私与安全”中允许打开。"
      : release.platform === "windows" || release.platform === "win32"
        ? "此安装包未签名，Windows 可能提示“未知发布者”。"
        : "此安装包未签名，安装或首次打开时可能需要完成系统安全确认。";
  return (
    <p className="osir-muted" role="note" aria-label="安装提示">
      {message}
    </p>
  );
}

export function OsirAnywhereEntry({ onDetails }: { onDetails: () => void }) {
  const state = useOsirAnywhere();
  return (
    <>
      <section
        className="osir-anywhere-entry"
        aria-label="OSIR Anywhere 入口"
        title={state.actionError ?? state.description}
      >
        <AnywhereIcon />
        <strong>OSIR Anywhere</strong>
        {state.actionError ? (
          <span className="osir-anywhere-entry-error" role="alert">
            操作未确认
          </span>
        ) : (
          <Status />
        )}
        <span className="osir-anywhere-entry-description">
          {state.actionError
            ? "打开或安装未确认"
            : state.label === "即将开放"
              ? "正式安装包尚未开放"
              : "跨设备继续 AI 任务"}
        </span>
        <DownloadProgress compact />
        <button className="osir-anywhere-details" onClick={onDetails}>
          查看详情
        </button>
        <Actions
          compact
          beforeInstall={
            state.release.data?.systemSignature === "unsigned"
              ? onDetails
              : undefined
          }
        />
      </section>
    </>
  );
}

function AnywherePage() {
  const state = useOsirAnywhere();
  const [webError, setWebError] = useState(false);
  const [openingWeb, setOpeningWeb] = useState(false);
  const codex = useQuery({
    queryKey: ["osir-anywhere-codex-status"],
    queryFn: () => invoke<CodexStatus>("osir_anywhere_codex_status"),
    enabled: state.desktop && !!state.installed,
    retry: false,
    staleTime: 15_000,
  });
  async function openWeb() {
    if (!state.desktop || openingWeb) return;
    setWebError(false);
    setOpeningWeb(true);
    try {
      await settingsApi.openExternal(ANYWHERE_WEB_URL);
    } catch {
      setWebError(true);
    } finally {
      setOpeningWeb(false);
    }
  }
  const release = !state.release.isError ? state.release.data : undefined;
  return (
    <section className="osir-anywhere-page" aria-label="OSIR Anywhere">
      <header className="osir-header">
        <div>
          <h1>OSIR Anywhere</h1>
          <p>跨设备查看进度，继续 AI 任务</p>
        </div>
        <button
          className="osir-anywhere-detect"
          disabled={!state.desktop || !!state.action || state.local.isFetching}
          aria-label="重新检测 OSIR Anywhere"
          title="重新检测"
          onClick={() => void state.redetect()}
        >
          <RefreshCw
            size={16}
            className={state.local.isFetching ? "animate-spin" : undefined}
          />
        </button>
      </header>
      <section className="osir-anywhere-hero" aria-label="安装状态">
        <AnywhereIcon large />
        <div className="osir-anywhere-summary">
          <h2>OSIR Anywhere</h2>
          <Status />
          <p>{state.description}</p>
          {!state.installed && release?.status === "ready" && (
            <p className="osir-anywhere-package">
              {[
                release.version
                  ? `v${release.version.replace(/^v/, "")}`
                  : null,
                release.platform === "darwin" || release.platform === "macos"
                  ? "macOS"
                  : release.platform === "win32" ||
                      release.platform === "windows"
                    ? "Windows"
                    : release.platform,
                release.arch === "universal"
                  ? "通用版"
                  : release.arch === "arm64"
                    ? release.platform === "darwin" ||
                      release.platform === "macos"
                      ? "Apple Silicon"
                      : "ARM64"
                    : release.arch === "x64"
                      ? release.platform === "darwin" ||
                        release.platform === "macos"
                        ? "Intel"
                        : "64 位"
                      : release.arch,
                release.size == null ? null : anywhereBytes(release.size),
              ]
                .filter(Boolean)
                .join(" · ")}
            </p>
          )}
          <UnsignedInstallNotice />
        </div>
        <Actions />
        {!state.installed && !state.detectionFailed && <DownloadProgress />}
      </section>
      {state.actionError && (
        <p className="osir-anywhere-error" role="alert">
          {state.actionError}
        </p>
      )}
      <section className="osir-anywhere-guide" aria-label="开始使用">
        <h2>开始使用</h2>
        <ol>
          <li>
            <span>1</span>
            <div>
              <h3>登录 OSIR 账号</h3>
              <p>在 Anywhere 中通过浏览器完成账号授权。</p>
            </div>
          </li>
          <li>
            <span>2</span>
            <div>
              <h3>选择项目并连接</h3>
              <p>选择允许远程访问的项目。</p>
            </div>
          </li>
          <li>
            <span>3</span>
            <div>
              <h3>通过浏览器继续对话</h3>
              <p>连接完成后，即可在其他设备继续。</p>
            </div>
          </li>
        </ol>
      </section>
      <section className="osir-anywhere-web" aria-label="网页访问">
        <Globe size={25} aria-hidden="true" />
        <div>
          <h2>网页访问</h2>
          <p>在其他设备打开网页版，登录同一账号。</p>
        </div>
        <button
          disabled={!state.desktop || openingWeb}
          onClick={() => void openWeb()}
        >
          {openingWeb ? "正在打开" : "打开网页版"}
          <ExternalLink size={14} />
        </button>
        {webError && <p role="alert">网页版打开失败，请重试。</p>}
      </section>
      <p className="osir-anywhere-note">
        <Info size={16} aria-hidden="true" />
        远程使用时，请保持这台电脑在线。
      </p>
      {state.displayPath && (
        <details className="osir-anywhere-local-config">
          <summary>本机安装与 Codex 配置</summary>
          <p>
            {state.detectionFailed ? "上次检测：" : "安装位置："}
            {state.displayPath}
          </p>
          {state.installed && (
            <>
              <p role="status">
                {codex.isFetching
                  ? "正在检查本机配置…"
                  : codex.isError || codex.data?.configured == null
                    ? "暂时无法确认配置，请重新检测。"
                    : codex.data.configured
                      ? "已有配置，对话可用性待验证。"
                      : "尚未检测到配置，请先配置 Codex。"}
              </p>
              {!codex.isError && codex.data && (
                <>
                  {codex.data.configRoot && (
                    <p>配置目录：{codex.data.configRoot}</p>
                  )}
                  <p>
                    {codex.data.connectionMode === "proxy"
                      ? codex.data.proxyRunning
                        ? "通过 OSIR 客户端连接，使用期间请保持客户端运行。"
                        : "本机代理尚未运行，请先恢复 Codex 连接。"
                      : codex.data.connectionMode === "direct"
                        ? "使用本机直连配置。"
                        : "连接方式暂未确认。"}
                  </p>
                  <p>
                    {codex.data.cliStatus === "installed" &&
                    codex.data.cliVersion
                      ? `Codex CLI v${codex.data.cliVersion}`
                      : codex.data.cliStatus === "not_installed"
                        ? "未检测到 Codex CLI。"
                        : "Codex CLI 版本暂未确认。"}
                  </p>
                </>
              )}
            </>
          )}
        </details>
      )}
    </section>
  );
}

// The workbench owns one provider so requests and downloads survive navigation.
// A standalone desktop prop keeps isolated component previews/tests usable.
export function OsirAnywhereProduct({ desktop }: { desktop?: boolean }) {
  return desktop === undefined ? (
    <AnywherePage />
  ) : (
    <OsirAnywhereProvider desktop={desktop}>
      <AnywherePage />
    </OsirAnywhereProvider>
  );
}
