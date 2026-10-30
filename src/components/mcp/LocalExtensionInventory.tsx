import { OsirLoading } from "@/components/ui/osir-loading";
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw, Search } from "lucide-react";
import { importLocalMcp, scanLocalExtensions } from "@/lib/api/localExtensions";
import { V7ConfirmDialog } from "./formBits";
import type { UnmanagedSkill } from "@/lib/api/skills";
import "@/features/osir/local-extensions.css";

const appNames: Record<string, string> = {
  codex: "Codex",
  claude: "Claude Code",
  "claude-desktop": "Claude Desktop",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  agents: "共享技能",
  "codex-system": "Codex 内置",
  "codex-plugins": "Codex 插件缓存",
  "claude-plugins": "Claude 插件缓存",
  "claude-project": "Claude 项目",
  "codex-project": "Codex 项目",
  "agents-project": "项目共享技能",
  "cc-switch": "管理目录",
  osir_client: "管理目录",
};
export const localSourceName = (source: string) => appNames[source] ?? source;

type LocalInventoryProps = {
  kind: "mcp" | "skills";
  skills?: UnmanagedSkill[];
  loading?: boolean;
  failed?: boolean;
  onRefresh?: () => void;
  onImport?: () => void;
  disabled?: boolean;
  roomy?: boolean;
  onPendingChange?: (pending: boolean) => void;
};

export function LocalExtensionInventory(props: LocalInventoryProps) {
  if (!("__TAURI_INTERNALS__" in window)) return null;
  return <NativeLocalInventory {...props} />;
}

function NativeLocalInventory({
  kind,
  skills,
  loading,
  failed,
  onRefresh,
  onImport,
  disabled = false,
  roomy = false,
  onPendingChange,
}: LocalInventoryProps) {
  const [search, setSearch] = useState("");
  const [importId, setImportId] = useState<string | null>(null);
  const queryClient = useQueryClient();
  const importing = useMutation({
    mutationFn: importLocalMcp,
    onSuccess: async () => {
      setImportId(null);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["osir-local-extensions"] }),
        queryClient.invalidateQueries({ queryKey: ["mcp", "all"] }),
      ]);
    },
  });
  const native = "__TAURI_INTERNALS__" in window;
  useEffect(() => {
    onPendingChange?.(importing.isPending);
    return () => onPendingChange?.(false);
  }, [importing.isPending, onPendingChange]);
  const scan = useQuery({
    queryKey: ["osir-local-extensions"],
    queryFn: scanLocalExtensions,
    enabled: native && kind === "mcp",
    staleTime: 30_000,
    retry: false,
  });
  const rows =
    kind === "mcp"
      ? (scan.data?.mcp ?? []).map((entry) => ({
          id: entry.id,
          name: entry.name,
          path: entry.sourcePath,
          source: `${localSourceName(entry.app)}${entry.scope === "project" ? " · 项目" : ""}`,
          state: entry.managed
            ? "已纳入管理"
            : entry.configured
              ? "本机已配置"
              : "本机已停用",
          importable: !entry.managed,
        }))
      : (skills ?? []).map((skill) => ({
          id: skill.path,
          name: skill.name,
          path: skill.path,
          source: skill.foundIn.map(localSourceName).join("、"),
          state: "本机已有",
          importable: false,
        }));
  const query = search.trim().toLowerCase();
  const filtered = rows.filter(
    (row) =>
      !query ||
      [row.name, row.path, row.source].some((value) =>
        value.toLowerCase().includes(query),
      ),
  );
  const pending = kind === "mcp" ? scan.isFetching : loading;
  const error = kind === "mcp" ? scan.isError : failed;
  const issues =
    kind === "mcp"
      ? (scan.data?.sources.filter((source) => source.status !== "loaded") ??
        [])
      : [];
  if (!native) return null;
  return (
    <>
      <section
        id={`osir-local-${kind}`}
        tabIndex={-1}
        className="osir-local-extensions osir-load-reveal"
        data-roomy={roomy || undefined}
        aria-label={kind === "mcp" ? "本机 MCP 服务" : "本机技能"}
      >
        <header>
          <div>
            <h2>
              本机已有 <span>{rows.length}</span>
            </h2>
            <p>
              {kind === "mcp"
                ? "来自工具和项目配置，服务状态以对应工具为准。"
                : "来自工具、共享目录和插件缓存，可选择导入。"}
            </p>
          </div>
          {kind === "skills" && rows.length > 0 && (
            <button disabled={disabled || pending} onClick={onImport}>
              选择导入
            </button>
          )}
          <button
            disabled={disabled || pending}
            onClick={() =>
              kind === "mcp" ? void scan.refetch() : onRefresh?.()
            }
            aria-label="重新扫描本机扩展"
          >
            <RefreshCw size={15} className={pending ? "animate-spin" : ""} />
          </button>
        </header>
        {rows.length > 0 && (
          <label className="osir-local-search">
            <Search size={14} />
            <input
              aria-label="搜索本机扩展"
              placeholder="搜索名称或来源"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          </label>
        )}
        {error ? (
          <p role="alert">无法读取本机扩展，请重新扫描。</p>
        ) : pending && rows.length === 0 ? (
          <OsirLoading label="正在读取本机扩展…" />
        ) : rows.length === 0 ? (
          <p className="osir-local-empty">
            未发现本机{kind === "mcp" ? " MCP 配置" : "技能"}。
          </p>
        ) : (
          <ul>
            {filtered.map((row) => (
              <li key={row.id}>
                <div>
                  <strong>{row.name}</strong>
                  <span>{row.source}</span>
                  <code title={row.path}>{row.path}</code>
                </div>
                <span className="osir-local-state">{row.state}</span>
                {row.importable && (
                  <button
                    disabled={disabled || importing.isPending}
                    onClick={() => {
                      importing.reset();
                      setImportId(row.id);
                    }}
                  >
                    导入
                  </button>
                )}
              </li>
            ))}
            {filtered.length === 0 && <li>没有匹配的扩展。</li>}
          </ul>
        )}
        {issues.length > 0 && (
          <details>
            <summary>{issues.length} 个配置未能读取</summary>
            {issues.map((source) => (
              <p key={source.path}>
                <code>{source.path}</code> ·{" "}
                {source.status === "invalid"
                  ? "格式不受支持或文件过大"
                  : "无法访问，请检查文件权限"}
              </p>
            ))}
          </details>
        )}
      </section>
      <V7ConfirmDialog
        open={importId !== null}
        title="导入 MCP 服务？"
        body="导入后可选择在哪些工具中启用。现有工具配置不会改变。"
        details={
          importing.isError ? (
            <p role="alert">导入失败，请重新扫描并检查是否已有同名服务。</p>
          ) : undefined
        }
        confirmLabel={importing.isError ? "重试导入" : "确认导入"}
        danger={false}
        pending={importing.isPending}
        onCancel={() => setImportId(null)}
        onConfirm={() => {
          if (importId) importing.mutate(importId);
        }}
      />
    </>
  );
}
