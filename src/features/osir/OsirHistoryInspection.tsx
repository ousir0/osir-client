import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createPortal } from "react-dom";
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  GitBranch,
  History,
  RefreshCw,
  ShieldCheck,
} from "lucide-react";
import { toast } from "@/lib/toast";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetTrigger,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
  SheetBody,
  SheetFooter,
} from "@/components/ui/sheet";
import type { SessionMeta } from "@/types";
import "./history-inspection.css";

type Inspection = {
  routeMatchesIndex: boolean;
  offsetsMatchRollout: boolean | null;
  checkedOffsets: number;
  rolloutRecords: number;
  canRepair: boolean;
};
const reasons: Record<string, string> = {
  unsafePath: "历史路径不满足安全检查要求。",
  unavailable: "本机历史或配置暂不可读取。",
  tooLarge: "会话记录过大，暂无法检查。",
  unstable: "会话记录暂不可读取，请退出 Codex 后重试。",
  unsupportedSchema: "当前历史或配置结构不在支持范围内。",
  invalidRollout: "历史记录不完整或结构尚不支持。",
  threadNotFound: "本机索引中没有找到这个会话。",
  databaseUnreadable: "无法读取历史索引。",
};
const repairReasons: Record<string, string> = {
  unsupported_platform: "当前修复仅支持 macOS。",
  writers_running:
    "检测到 Codex Desktop、CLI 或 app-server 仍在运行，请全部退出后重试。",
  process_check_failed: "无法确认 Codex 已全部退出，暂未修复。请检查后重试。",
  busy: "会话记录暂不可修改，请退出 Codex 后重试；请勿手动删除相关文件。",
  unsafe_path: "历史路径或文件权限未通过安全校验。",
  too_large: "会话记录过大，暂无法修复。",
  unsupported_schema: "当前会话记录格式暂不支持修复。",
  invalid_rollout: "历史记录不完整或含不支持的结构。",
  unmapped_route: "无法确认此会话原来使用的连接，暂未修复。",
  changed: "历史或配置已变化，预览失效，请重新检查。",
  pending_recovery:
    "操作尚未完成，已保留备份。请保持 Codex 关闭，刷新恢复记录。",
  recovery_conflict:
    "当前历史与备份不再一致，已停止恢复以保护新内容。请保留备份。",
  storage: "文件操作未完成，请刷新恢复记录查看保留状态。",
  no_changes: "此会话已使用所选连接，无需修复。",
};
function failure(error: unknown): string {
  if (typeof error === "object" && error !== null && "code" in error) {
    if (error.code === "busy")
      return "另一个历史检查正在运行，请稍后手动重试。";
    if (error.code === "invalidThreadId")
      return "会话标识无效，请重新选择会话。";
    if (error.code === "targetUnavailable")
      return "所选连接或模型已变化，请刷新后重新选择。";
    if (error.code === "previewExpired")
      return "预览已过期或已使用，请重新生成预览。";
    if (error.code === "invalidConfirmation")
      return "请先确认修复范围，并退出所有 Codex 客户端和终端会话。";
    if (
      error.code === "repair" &&
      "reason" in error &&
      typeof error.reason === "string"
    )
      return repairReasons[error.reason] ?? "修复未确认，请刷新恢复记录。";
    if (
      error.code === "inspection" &&
      "reason" in error &&
      typeof error.reason === "string"
    )
      return reasons[error.reason] ?? "无法确定兼容性，请稍后重试。";
  }
  return "检查未完成，无法确定兼容性，请稍后重试。";
}
function valid(value: Inspection): boolean {
  return (
    value != null &&
    typeof value.routeMatchesIndex === "boolean" &&
    (value.offsetsMatchRollout === null ||
      typeof value.offsetsMatchRollout === "boolean") &&
    Number.isSafeInteger(value.checkedOffsets) &&
    value.checkedOffsets >= 0 &&
    Number.isSafeInteger(value.rolloutRecords) &&
    value.rolloutRecords >= 0
  );
}

/** Remount per selected session so a late response cannot become another session's result. */
export function OsirHistoryInspection({
  session,
  desktop,
}: {
  session: SessionMeta;
  desktop: boolean;
}) {
  if (session.providerId !== "codex") return null;
  return (
    <InspectionPanel
      key={JSON.stringify([
        session.providerId,
        session.sessionId,
        desktop,
        session.readOnly,
      ])}
      threadId={session.sessionId}
      desktop={desktop}
      readOnly={session.readOnly ?? false}
    />
  );
}
function InspectionPanel({
  threadId,
  desktop,
  readOnly,
}: {
  threadId: string;
  desktop: boolean;
  readOnly: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [repairBusy, setRepairBusy] = useState(false);
  const [footerHost, setFooterHost] = useState<HTMLDivElement | null>(null);
  const [result, setResult] = useState<Inspection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(false);
  const inFlight = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  async function inspect() {
    if (!desktop || !threadId || inFlight.current || repairBusy) return;
    inFlight.current = true;
    setPending(true);
    setResult(null);
    setError(null);
    try {
      const response = await invoke<Inspection>("osir_inspect_codex_history", {
        threadId,
      });
      if (!valid(response)) throw new Error("unsupported response");
      if (alive.current) setResult(response);
    } catch (error) {
      if (alive.current) setError(failure(error));
    } finally {
      inFlight.current = false;
      if (alive.current) setPending(false);
    }
  }
  return (
    <section aria-label="历史会话续聊" className="osir-history-entry">
      <header>
        <h2>
          <History size={16} aria-hidden="true" />
          历史会话续聊
        </h2>
        <p>
          {readOnly
            ? "此来源只读，可检查连接兼容性；请在 Codex Desktop 中继续。"
            : "检查当前会话的连接信息，确认映射后再继续。"}
        </p>
      </header>
      <Sheet
        open={open}
        onOpenChange={(next) => {
          if (!next && (pending || repairBusy)) return;
          if (next) {
            setResult(null);
            setError(null);
          }
          setOpen(next);
        }}
      >
        <SheetTrigger asChild>
          <Button
            variant="neutral"
            size="regular"
            className="osir-history-primary"
            disabled={!threadId || pending}
          >
            <ShieldCheck size={15} aria-hidden="true" />
            {pending ? "检查中…" : "检查续聊兼容性"}
          </Button>
        </SheetTrigger>
        <SheetContent
          width={420}
          closeLabel="关闭历史会话续聊"
          className="osir-drawer osir-history-sheet"
          aria-busy={pending || repairBusy}
          onEscapeKeyDown={(event) => {
            if (pending || repairBusy) event.preventDefault();
          }}
          onKeyDown={(event) => event.stopPropagation()}
        >
          <SheetHeader className="osir-history-header">
            <SheetTitle>历史会话续聊</SheetTitle>
            <SheetDescription>
              {readOnly
                ? "检查当前会话的连接兼容性。此来源只读，不提供修复操作。"
                : "将旧连接映射到本机当前连接，检查并确认后再修复。"}
            </SheetDescription>
          </SheetHeader>
          <SheetBody className="osir-history-body">
            <section
              className="osir-history-section"
              aria-labelledby="osir-history-check-title"
            >
              <div className="osir-history-check-card">
                <div className="osir-history-heading">
                  <h3 id="osir-history-check-title">
                    <span className="osir-history-step" aria-hidden="true">
                      1
                    </span>
                    当前状态
                  </h3>
                  <span className="osir-history-badge">
                    {result
                      ? "已检查"
                      : pending
                        ? "检查中"
                        : error
                          ? "检查未完成"
                          : "尚未检查"}
                  </span>
                </div>
                {desktop && (
                  <Button
                    variant="neutral"
                    size="regular"
                    className="osir-history-primary"
                    disabled={pending || repairBusy}
                    onClick={() => void inspect()}
                  >
                    {pending
                      ? "检查中…"
                      : result || error
                        ? "重新检查会话"
                        : "检查会话"}
                  </Button>
                )}
              </div>
              {!desktop ? (
                <p className="osir-history-hint">
                  请在桌面客户端检查；浏览器预览不会读取本机历史。
                </p>
              ) : (
                <>
                  <p className="osir-history-hint">
                    仅检查当前会话的连接信息，不会修改历史或显示对话正文。
                  </p>
                  {pending && (
                    <p
                      role="status"
                      aria-live="polite"
                      className="osir-history-message"
                    >
                      正在检查会话记录…
                    </p>
                  )}
                  {error && (
                    <p role="alert" className="osir-history-message">
                      {error}
                    </p>
                  )}
                  {result && (
                    <div
                      role="status"
                      aria-live="polite"
                      className="osir-history-result"
                    >
                      <p
                        className="osir-history-result-row"
                        data-match={result.routeMatchesIndex ? "true" : "false"}
                      >
                        {result.routeMatchesIndex ? (
                          <CheckCircle2 size={16} aria-hidden="true" />
                        ) : (
                          <AlertTriangle size={16} aria-hidden="true" />
                        )}
                        {result.routeMatchesIndex
                          ? "会话连接信息一致。"
                          : "会话连接信息不一致或暂时无法确认。"}
                      </p>
                      <p
                        className="osir-history-result-row"
                        data-match={
                          result.offsetsMatchRollout === true ? "true" : "false"
                        }
                      >
                        {result.offsetsMatchRollout === true ? (
                          <CheckCircle2 size={16} aria-hidden="true" />
                        ) : (
                          <AlertTriangle size={16} aria-hidden="true" />
                        )}
                        {result.offsetsMatchRollout === null
                          ? "未找到历史缓存索引，暂无法核对。"
                          : result.offsetsMatchRollout
                            ? "会话记录与索引位置一致。"
                            : "会话记录与索引位置不一致，暂无法确认是否可续聊。"}
                      </p>
                      <p>
                        已核对 {result.rolloutRecords} 条记录、
                        {result.checkedOffsets} 处索引位置。实际续聊情况请在
                        Codex 中确认。
                      </p>
                    </div>
                  )}
                </>
              )}
            </section>
            {desktop && !readOnly && (
              <HistoryRepairPanel
                key={open ? "open" : "closed"}
                threadId={threadId}
                inspectionBusy={pending}
                inspectionComplete={!!result && !error}
                onBusyChange={setRepairBusy}
                onHistoryChanged={() => setResult(null)}
                footerHost={footerHost}
              />
            )}
          </SheetBody>
          <SheetFooter className="osir-history-footer">
            <div className="osir-history-actions">
              <Button
                variant="neutral"
                size="regular"
                disabled={pending || repairBusy}
                onClick={() => setOpen(false)}
              >
                取消
              </Button>
              <div ref={setFooterHost} />
            </div>
            <p className="osir-history-hint">
              {pending || repairBusy
                ? "检查或本机操作完成前，续聊面板保持打开。"
                : readOnly
                  ? "此来源只读，兼容性检查不会修改原有会话。"
                  : "预览不会修改历史；修复前需要退出 Codex 写入进程并再次确认。"}
            </p>
          </SheetFooter>
        </SheetContent>
      </Sheet>
    </section>
  );
}

type Route = { provider: string; model: string };
type Target = { providerId: string; providerName: string; modelId: string };
type Options = { source: Route; targets: Target[] };
function validRoute(route: Route): boolean {
  return (
    !!route &&
    [route.provider, route.model].every(
      (value) =>
        typeof value === "string" &&
        value.length > 0 &&
        value.length <= 256 &&
        !/[\u0000-\u001f\u007f]/.test(value),
    )
  );
}
function validOptions(value: Options): boolean {
  return (
    !!value &&
    validRoute(value.source) &&
    Array.isArray(value.targets) &&
    value.targets.length <= 512 &&
    value.targets.every(
      (target) =>
        !!target &&
        [target.providerId, target.providerName, target.modelId].every(
          (field) =>
            typeof field === "string" &&
            field.length > 0 &&
            !/[\u0000-\u001f\u007f]/.test(field),
        ),
    )
  );
}
type RepairPreview = {
  previewId: string;
  expiresInSeconds: number;
  preview: {
    mapping: { threadId: string; source: Route; target: Route };
    changedRecords: number;
    changedOffsets: number;
    historyCachePresent: boolean;
  };
};
type SavedOperation = {
  operationId: string;
  state:
    | "pending"
    | "restoring"
    | "completed"
    | "restored"
    | "incomplete_backup";
};

function HistoryRepairPanel({
  threadId,
  inspectionBusy,
  inspectionComplete,
  onBusyChange,
  onHistoryChanged,
  footerHost,
}: {
  threadId: string;
  inspectionBusy: boolean;
  inspectionComplete: boolean;
  onBusyChange: (busy: boolean) => void;
  onHistoryChanged: () => void;
  footerHost: HTMLDivElement | null;
}) {
  const [options, setOptions] = useState<Options>();
  const [selected, setSelected] = useState("");
  const [preview, setPreview] = useState<RepairPreview>();
  const [operations, setOperations] = useState<SavedOperation[]>([]);
  const [closed, setClosed] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [recovery, setRecovery] = useState<{
    id: string;
    action: "complete" | "rollback";
  }>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [failed, setFailed] = useState(false);
  const [operationsLoaded, setOperationsLoaded] = useState(false);
  const alive = useRef(false);
  const inFlight = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    if (!inspectionBusy) return;
    setOptions(undefined);
    setSelected("");
    setPreview(undefined);
    setRecovery(undefined);
    setClosed(false);
    setConfirmed(false);
  }, [inspectionBusy]);
  useEffect(() => {
    if (!preview) return;
    const timeout = window.setTimeout(() => {
      setPreview(undefined);
      setConfirmed(false);
      setFailed(true);
      setMessage("预览已过期，请重新生成预览并确认。");
    }, preview.expiresInSeconds * 1000);
    return () => window.clearTimeout(timeout);
  }, [preview]);
  const disabled = busy || inspectionBusy;
  const target = options?.targets.find(
    (_, index) => String(index) === selected,
  );
  async function run(work: () => Promise<void>) {
    if (inFlight.current || inspectionBusy) return;
    inFlight.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    setFailed(false);
    try {
      await work();
    } catch (error) {
      if (alive.current) {
        setFailed(true);
        setMessage(failure(error));
      }
    } finally {
      inFlight.current = false;
      if (alive.current) {
        setBusy(false);
        onBusyChange(false);
      }
    }
  }
  async function loadOperations() {
    const saved = await invoke<SavedOperation[]>(
      "osir_codex_history_operations",
    );
    if (alive.current) {
      setOperations(saved);
      setOperationsLoaded(true);
      setRecovery(undefined);
      setConfirmed(false);
    }
  }
  return (
    <section
      className="osir-history-section"
      aria-labelledby="osir-history-mapping-title"
    >
      <div className="osir-history-heading">
        <h3 id="osir-history-mapping-title">
          <span className="osir-history-step" aria-hidden="true">
            2
          </span>
          连接信息映射
        </h3>
        <span className="osir-history-badge">本机配置</span>
      </div>
      <p className="osir-history-hint">
        来源与目标均由当前会话和本机已发布连接读取；未识别的来源不会自动匹配。
      </p>
      <div className="osir-history-actions">
        <Button
          variant="neutral"
          size="regular"
          disabled={disabled || !inspectionComplete}
          onClick={() =>
            void run(async () => {
              setPreview(undefined);
              setConfirmed(false);
              setOptions(undefined);
              setSelected("");
              const value = await invoke<Options>(
                "osir_codex_history_options",
                { threadId },
              );
              if (!validOptions(value)) throw new Error("unsupported mapping");
              if (alive.current) setOptions(value);
            })
          }
        >
          <GitBranch size={15} aria-hidden="true" />
          读取连接映射
        </Button>
        <Button
          variant="neutral"
          size="regular"
          disabled={disabled}
          onClick={() => void run(loadOperations)}
        >
          <RefreshCw size={15} aria-hidden="true" />
          刷新恢复记录
        </Button>
      </div>
      {!inspectionComplete && (
        <p className="osir-history-hint">
          完成只读检查后，再读取连接映射；恢复记录可独立查看。
        </p>
      )}
      {options && (
        <>
          <div className="osir-history-mapping">
            <div className="osir-history-route">
              <small>旧连接</small>
              <strong>{options.source.provider}</strong>
              <p>{options.source.model}</p>
            </div>
            <ArrowRight aria-hidden="true" size={16} />
            <div className="osir-history-route">
              <small>当前连接</small>
              {target ? (
                <>
                  <strong>{target.providerName}</strong>
                  <p>{target.modelId}</p>
                </>
              ) : (
                <p>选择本机已发布连接</p>
              )}
            </div>
          </div>
          {options.targets.length ? (
            <label className="osir-history-selector">
              选择当前连接与模型
              <select
                value={selected}
                disabled={disabled}
                onChange={(event) => {
                  setSelected(event.target.value);
                  setPreview(undefined);
                  setConfirmed(false);
                  setRecovery(undefined);
                }}
              >
                <option value="">请选择目标</option>
                {options.targets.map((item, index) => (
                  <option
                    key={JSON.stringify([item.providerId, item.modelId])}
                    value={String(index)}
                  >
                    {item.providerName} · {item.modelId}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <p className="osir-history-message">
              暂无可用连接，请先在供应商页面启用连接并添加模型。
            </p>
          )}
        </>
      )}
      {!options && (
        <p className="osir-history-hint">
          尚未读取连接信息，请先刷新连接列表。
        </p>
      )}
      <div className="osir-history-notice">
        <AlertTriangle aria-hidden="true" size={16} />
        <div>
          <h4>注意</h4>
          <p>
            连接修复与备份恢复仅支持 macOS。请先正常退出 Codex Desktop、CLI 和
            app-server，操作完成前保持关闭。检测到历史已变化时会停止覆盖。
          </p>
        </div>
      </div>
      <div className="osir-history-backup">
        <ShieldCheck aria-hidden="true" size={16} />
        <div>
          <h4>备份说明</h4>
          <p>
            修复前会自动备份，仅更新此会话的连接信息及相关索引，保留对话正文。备份保存在本机；若记录后续有变化，可能无法恢复到修复前。
          </p>
        </div>
      </div>
      <label className="osir-history-check">
        <input
          type="checkbox"
          checked={closed}
          disabled={disabled}
          onChange={(event) => {
            setClosed(event.target.checked);
            setConfirmed(false);
            setPreview(undefined);
          }}
        />
        我已退出所有 Codex 客户端和终端会话，并会保持关闭直到操作完成。
      </label>
      {options && (
        <Button
          variant="neutral"
          size="regular"
          disabled={disabled || !inspectionComplete || !target || !closed}
          onClick={() =>
            void run(async () => {
              if (!target) return;
              setPreview(undefined);
              setConfirmed(false);
              setRecovery(undefined);
              const value = await invoke<RepairPreview>(
                "osir_codex_history_preview",
                {
                  threadId,
                  providerId: target.providerId,
                  modelId: target.modelId,
                },
              );
              if (
                !value?.previewId ||
                !value.preview?.mapping ||
                value.preview.mapping.threadId !== threadId ||
                !validRoute(value.preview.mapping.source) ||
                !validRoute(value.preview.mapping.target) ||
                value.preview.mapping.target.model !== target.modelId ||
                !Number.isSafeInteger(value.preview.changedRecords) ||
                value.preview.changedRecords < 0 ||
                !Number.isSafeInteger(value.preview.changedOffsets) ||
                value.preview.changedOffsets < 0 ||
                !Number.isSafeInteger(value.expiresInSeconds) ||
                value.expiresInSeconds <= 0 ||
                value.expiresInSeconds > 300
              )
                throw new Error("unsupported preview");
              if (alive.current) setPreview(value);
            })
          }
        >
          预览连接修复
        </Button>
      )}
      {preview && !recovery && (
        <div className="osir-history-preview">
          <h4>修复预览</h4>
          <p>
            {preview.preview.mapping.source.provider} /{" "}
            {preview.preview.mapping.source.model} →{" "}
            {preview.preview.mapping.target.provider} /{" "}
            {preview.preview.mapping.target.model}
          </p>
          <p>
            将更新 {preview.preview.changedRecords} 条连接记录、
            {preview.preview.changedOffsets} 处索引位置。请在{" "}
            {Math.floor(preview.expiresInSeconds / 60)}
            分钟内确认，过期后需重新预览。
          </p>
          {!preview.preview.historyCachePresent && (
            <p>本机未发现历史缓存数据库，本次不会创建缓存。</p>
          )}
          <label className="osir-history-check">
            <input
              type="checkbox"
              disabled={disabled || !closed}
              checked={confirmed}
              onChange={(event) => setConfirmed(event.target.checked)}
            />
            我已确认原连接和目标连接，同意备份并修复此会话。
          </label>
        </div>
      )}
      {operationsLoaded && operations.length === 0 && (
        <p className="osir-history-hint">本机暂无修复恢复记录。</p>
      )}
      {operations.length > 0 && (
        <div className="osir-history-section">
          <h4>本机恢复记录</h4>
          <p className="osir-history-hint">
            以下包含当前 Codex
            配置目录中全部会话的修复记录，不限当前会话。请核对操作编号；恢复时会重新核对当前文件，避免覆盖后续新增内容。
          </p>
          {operations.map((op) => (
            <div key={op.operationId} className="osir-history-operation">
              <code className="break-all">{op.operationId}</code>
              <span>
                {
                  {
                    pending: "待恢复",
                    restoring: "回滚未完成",
                    completed: "已完成",
                    restored: "已回滚",
                    incomplete_backup: "备份未完成，未开始替换",
                  }[op.state]
                }
              </span>
              {op.state === "pending" && (
                <Button
                  variant="neutral"
                  size="regular"
                  disabled={disabled}
                  onClick={() => {
                    setPreview(undefined);
                    setRecovery({ id: op.operationId, action: "complete" });
                    setConfirmed(false);
                  }}
                >
                  选择继续修复
                </Button>
              )}
              {["pending", "restoring", "completed"].includes(op.state) && (
                <Button
                  variant="neutral"
                  size="regular"
                  disabled={disabled}
                  onClick={() => {
                    setPreview(undefined);
                    setRecovery({ id: op.operationId, action: "rollback" });
                    setConfirmed(false);
                  }}
                >
                  选择回滚备份
                </Button>
              )}
            </div>
          ))}
        </div>
      )}
      {recovery && (
        <div className="osir-history-preview">
          <p>
            {recovery.action === "complete" ? "继续完成修复" : "恢复修复前备份"}
            ：{recovery.id}
          </p>
          <label className="osir-history-check">
            <input
              type="checkbox"
              disabled={disabled || !closed}
              checked={confirmed}
              onChange={(event) => setConfirmed(event.target.checked)}
            />
            我确认对这条记录执行以上操作。
          </label>
        </div>
      )}
      {footerHost &&
        createPortal(
          <Button
            variant="neutral"
            size="regular"
            className="osir-history-primary"
            disabled={
              disabled ||
              !confirmed ||
              !closed ||
              (!recovery && (!preview || !inspectionComplete))
            }
            onClick={() =>
              void run(async () => {
                if (!closed || !confirmed) return;
                if (!recovery && preview && inspectionComplete) {
                  const id = preview.previewId;
                  setPreview(undefined);
                  setConfirmed(false);
                  onHistoryChanged();
                  await invoke("osir_codex_history_apply", {
                    previewId: id,
                    confirmed: true,
                  });
                  if (!alive.current) return;
                  setOptions(undefined);
                  setSelected("");
                  setClosed(false);
                  await loadOperations();
                  if (alive.current)
                    toast.success(
                      "连接信息已修复并校验，备份已保留。请回到 Codex 验证续聊；若需回滚，请先关闭 Codex。",
                    );
                  return;
                }
                if (!recovery) return;
                const chosen = recovery;
                setRecovery(undefined);
                setConfirmed(false);
                setPreview(undefined);
                onHistoryChanged();
                await invoke("osir_codex_history_recover", {
                  operationId: chosen.id,
                  action: chosen.action,
                  confirmed: true,
                });
                if (alive.current) {
                  setOptions(undefined);
                  setSelected("");
                  setClosed(false);
                  toast.success(
                    chosen.action === "rollback"
                      ? "已回滚并核对备份。"
                      : "已完成中断的修复并核对文件。",
                  );
                }
                await loadOperations();
              })
            }
          >
            {recovery ? "确认恢复操作" : "修复连接信息"}
          </Button>,
          footerHost,
        )}
      {(busy || (failed && message)) && (
        <p
          role={failed ? "alert" : "status"}
          aria-live="polite"
          className="osir-history-message"
        >
          {busy ? "正在处理会话记录，请保持 Codex 关闭…" : message}
        </p>
      )}
    </section>
  );
}
