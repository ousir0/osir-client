import {
  useCallback,
  useEffect,
  useId,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQueryClient } from "@tanstack/react-query";
import {
  Sheet,
  SheetTrigger,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
  SheetBody,
  SheetFooter,
  SheetClose,
} from "@/components/ui/sheet";
import { accountSessionKey, type AccountSession } from "./accountApi";
import { AccountWebsiteButton } from "./AccountOverview";
import "./key-details.css";

const toolNames = {
  codex: "Codex",
  claude_code: "Claude Code",
  open_code: "OpenCode",
  gemini_cli: "Gemini CLI",
} as const;
type Tool = keyof typeof toolNames;
export type KeyUsageTool = "codex" | "claude" | "opencode" | "gemini";
const toolPages: Record<Tool, KeyUsageTool> = {
  codex: "codex",
  claude_code: "claude",
  open_code: "opencode",
  gemini_cli: "gemini",
};
type LocalUsage = { tool: Tool; providerId: string; providerName: string };
type ReadState = { generation: number; keyId: number } & (
  | { phase: "loading" | "error" }
  | { phase: "ready"; locations: LocalUsage[] }
);
const unavailable =
  "当前桌面授权不支持修改密钥名称或启停，请前往 OSIR 网站管理。";

function object(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Only the native allowlisted location DTO survives into component state. */
function locations(
  value: unknown,
  generation: number,
  keyId: number,
): LocalUsage[] {
  if (
    !object(value) ||
    value.generation !== generation ||
    value.keyId !== keyId ||
    value.source !== "saved_provider_metadata" ||
    !object(value.capabilities) ||
    value.capabilities.rename !== false ||
    value.capabilities.changeStatus !== false ||
    !Array.isArray(value.localUsages)
  )
    throw new Error("无法确认密钥详情");
  return value.localUsages.map((item: unknown) => {
    if (
      !object(item) ||
      typeof item.tool !== "string" ||
      !Object.prototype.hasOwnProperty.call(toolNames, item.tool) ||
      typeof item.providerId !== "string" ||
      !item.providerId ||
      typeof item.providerName !== "string"
    )
      throw new Error("无法确认本机引用");
    return {
      tool: item.tool as Tool,
      providerId: item.providerId,
      providerName: item.providerName,
    };
  });
}

/** No key bodies, generic provider reads, remote mutation, or automatic retry. */
export function OsirKeyDetails({
  generation,
  keyId,
  status,
  onOpenTool,
  children,
}: {
  generation: number;
  keyId: number;
  status?: string;
  onOpenTool?: (tool: KeyUsageTool) => void;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<ReadState>();
  const reasonId = useId();
  const client = useQueryClient();
  // Observe the existing session cache; the parent owns native session reads.
  const subscribe = useCallback(
    (notify: () => void) => {
      if (!open) return () => {};
      return client.getQueryCache().subscribe((event) => {
        if (
          event.query.queryKey[0] === accountSessionKey[0] &&
          event.query.queryKey[1] === accountSessionKey[1]
        )
          notify();
      });
    },
    [client, open],
  );
  const snapshot = useCallback(
    () => client.getQueryData<AccountSession>(accountSessionKey),
    [client],
  );
  const session = useSyncExternalStore(subscribe, snapshot, snapshot);
  const current =
    session?.connected === true && session.generation === generation;
  const native =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  useEffect(() => {
    if (!open || !current || !native) {
      setRead(undefined);
      return;
    }
    let cancelled = false;
    const stillCurrent = () => {
      const active = client.getQueryData<AccountSession>(accountSessionKey);
      return (
        !cancelled &&
        active?.connected === true &&
        active.generation === generation
      );
    };
    setRead({ phase: "loading", generation, keyId });
    void invoke<unknown>("osir_key_management", { generation, keyId })
      .then((reply) => {
        if (!stillCurrent()) return;
        setRead({
          phase: "ready",
          generation,
          keyId,
          locations: locations(reply, generation, keyId),
        });
      })
      .catch(() => {
        // Raw native errors may contain confidential text; use fixed copy.
        if (stillCurrent()) setRead({ phase: "error", generation, keyId });
      });
    return () => {
      cancelled = true;
    };
  }, [open, current, native, client, generation, keyId, attempt]);

  const visibleRead =
    read?.generation === generation && read.keyId === keyId ? read : undefined;
  const busy = !visibleRead || visibleRead.phase === "loading";
  const stateAction =
    status === "active"
      ? "禁用密钥"
      : status === "inactive"
        ? "启用密钥"
        : "启用 / 禁用";

  return (
    <Sheet open={open} onOpenChange={setOpen}>
      <SheetTrigger asChild>
        <button type="button" disabled={!current}>
          密钥详情
        </button>
      </SheetTrigger>
      <SheetContent
        className="osir-drawer osir-account-detail-drawer osir-key-details"
        width={420}
        closeLabel="关闭密钥详情"
        dismissOnOutsideClick
      >
        <SheetHeader>
          <SheetTitle>密钥详情</SheetTitle>
          <SheetDescription>
            查看密钥限制与本机已保存的连接引用。
          </SheetDescription>
        </SheetHeader>
        <SheetBody>
          {!current ? (
            <p role="alert">账户会话已改变，请关闭详情后重新同步。</p>
          ) : (
            <>
              {children}
              <section
                className="osir-detail-section"
                aria-label="本机使用位置"
              >
                <h4>本机使用位置</h4>
                <p className="osir-muted">
                  仅根据本机已保存的连接记录匹配，不代表工具正在运行或当前启用；不包含其他设备及独立保存的生图配置。
                </p>
                {!native ? (
                  <p role="status">本机使用位置仅可在桌面客户端读取。</p>
                ) : (
                  <>
                    {busy && <p role="status">正在读取本机已保存连接…</p>}
                    {visibleRead?.phase === "error" && (
                      <p role="alert">
                        本机使用位置读取失败。请手动重试；未更改任何密钥或连接。
                      </p>
                    )}
                    {visibleRead?.phase === "ready" &&
                      (visibleRead.locations.length ? (
                        <ul className="osir-key-locations">
                          {visibleRead.locations.map((item) => (
                            <li key={item.tool + ":" + item.providerId}>
                              <strong>{toolNames[item.tool]}</strong>
                              <span>
                                {item.providerName.trim() || "未命名连接"}
                              </span>
                              <div className="osir-key-location-actions">
                                <small>已保存引用</small>
                                {onOpenTool && (
                                  <button
                                    type="button"
                                    aria-label={
                                      "查看 " +
                                      toolNames[item.tool] +
                                      " 的 " +
                                      item.providerName +
                                      " 配置"
                                    }
                                    onClick={() => {
                                      setOpen(false);
                                      onOpenTool(toolPages[item.tool]);
                                    }}
                                  >
                                    查看配置
                                  </button>
                                )}
                              </div>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <p role="status">未发现本机已保存连接引用。</p>
                      ))}
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => setAttempt((n) => n + 1)}
                    >
                      {visibleRead?.phase === "error"
                        ? "重试读取本机位置"
                        : "刷新本机引用"}
                    </button>
                  </>
                )}
              </section>
              <section className="osir-detail-section" aria-label="密钥管理">
                <h4>密钥管理</h4>
                <p id={reasonId}>{unavailable}</p>
                <AccountWebsiteButton label="前往 OSIR 网站管理密钥" />
                <p className="osir-key-impact">
                  禁用会影响所有使用此密钥的客户端，包括未列出的连接和其他设备；启用不会自动启动工具。执行前请核对影响范围。
                </p>
              </section>
            </>
          )}
        </SheetBody>
        <SheetFooter>
          <button
            type="button"
            disabled
            aria-describedby={current ? reasonId : undefined}
          >
            修改名称
          </button>
          <button
            type="button"
            disabled
            aria-describedby={current ? reasonId : undefined}
          >
            {stateAction}
          </button>
          <SheetClose asChild>
            <button type="button">关闭</button>
          </SheetClose>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
