import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import { Image, KeyRound, Layers, LockKeyhole } from "lucide-react";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "@/lib/toast";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
  SheetBody,
  SheetFooter,
  SheetTrigger,
} from "@/components/ui/sheet";
import { useAccountSession, useAccountResource } from "./accountApi";

type ImageConfig = {
  status: string;
  reason?: string;
  selection?: {
    source: "saved" | "custom";
    provider_id?: string;
    endpoint: string;
    model: string;
    has_key: boolean;
  };
  candidates: {
    provider_id: string;
    name: string;
    masked_key: string;
    group_id?: number;
  }[];
};

/** One Codex-wide selection. Secrets are input-only and stored by the native service. */
export function OsirImageBindingPanel({
  openRequest = 0,
  onOpenHandled,
}: {
  openRequest?: number;
  onOpenHandled?: () => void;
}) {
  const session = useAccountSession();
  const query = useQueryClient();
  const generation = session.data?.generation;
  const desktop =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const config = useQuery({
    queryKey: ["osir-global-image", generation],
    enabled: desktop && !!session.data?.connected,
    queryFn: async () => {
      const current = await invoke<ImageConfig>("osir_image_get");
      return current.selection
        ? current
        : invoke<ImageConfig>("osir_image_auto_default", { generation });
    },
    staleTime: 60_000,
    retry: false,
    refetchOnWindowFocus: false,
  });
  const [dirty, setDirty] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  const [changing, setChanging] = useState(false);
  const [editing, setEditing] = useState(false);
  const [source, setSource] = useState<"saved" | "custom">("saved");
  const [providerId, setProviderId] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [model, setModel] = useState("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const selected = config.data?.selection;
  const groups = useAccountResource<{ id: number; name?: string }[]>(
    "groups",
    {},
    !!session.data?.connected,
  );
  const savedCandidate = config.data?.candidates.find(
    (p) => p.provider_id === selected?.provider_id,
  );
  const draftCandidate =
    source === "saved"
      ? config.data?.candidates.find((p) => p.provider_id === providerId)
      : undefined;
  const currentGroups =
    groups.data && groups.data.generation === generation
      ? groups.data.data
      : undefined;
  const savedGroup = currentGroups?.find(
    (g) => g.id === savedCandidate?.group_id,
  );
  const draftGroup = currentGroups?.find(
    (g) => g.id === draftCandidate?.group_id,
  );
  const showDraft = changing || dirty || !selected;
  const summarySource = showDraft ? source : selected?.source || "saved";
  const summaryCandidate = showDraft ? draftCandidate : savedCandidate;
  const sameSavedKey =
    summarySource === "saved" &&
    selected?.source === "saved" &&
    summaryCandidate?.provider_id === selected.provider_id;
  const summaryModel = showDraft
    ? source === "custom"
      ? model || "尚未填写"
      : sameSavedKey
        ? selected?.model || "后台配置"
        : summaryCandidate
          ? "由所选密钥决定"
          : "选择密钥后确定"
    : selected?.model || "后台配置";
  const summaryEndpoint = showDraft
    ? source === "custom"
      ? endpoint || "尚未填写"
      : sameSavedKey
        ? selected?.endpoint || "尚未配置"
        : summaryCandidate
          ? "由所选密钥决定"
          : "选择密钥后确定"
    : selected?.endpoint || "尚未配置";

  function groupLabel(
    candidate: ImageConfig["candidates"][number] | undefined,
    valueSource: "saved" | "custom",
    draft: boolean,
  ) {
    if (valueSource === "custom") return "自定义接口（无 OSIR 分组）";
    if (!candidate) {
      if (draft && !providerId) return "选择密钥后显示";
      return config.isError ? "密钥信息读取失败" : "分组信息不可用";
    }
    if (candidate.group_id == null) return "该密钥未关联分组";
    const match = (draft ? draftGroup : savedGroup)?.name;
    if (match) return match;
    if (groups.isError) return "分组信息读取失败";
    if (groups.isPending) return "正在读取分组信息…";
    return "分组 #" + candidate.group_id;
  }
  const summaryKey =
    summarySource === "custom"
      ? showDraft
        ? key
          ? "已输入新密钥"
          : selected?.source === "custom" && selected.has_key
            ? "需重新输入密钥"
            : "尚未输入密钥"
        : selected?.has_key
          ? "已安全保存"
          : "尚未配置"
      : summaryCandidate
        ? summaryCandidate.masked_key
        : showDraft && !providerId
          ? "尚未选择密钥"
          : config.isError
            ? "密钥信息读取失败"
            : "密钥信息不可用";
  const summaryName =
    summarySource === "custom"
      ? "自定义生图密钥"
      : summaryCandidate?.name ||
        (showDraft && !providerId ? "尚未选择密钥" : "密钥不可用");
  const savedCaption = selected
    ? selected.source === "custom"
      ? [selected.model, "自定义密钥", selected.endpoint]
          .filter(Boolean)
          .join(" · ")
      : [selected.model, savedCandidate?.name, savedCandidate?.masked_key]
          .filter(Boolean)
          .join(" · ") || "已保存的密钥不可用"
    : "默认使用支持生图的 OpenAI 分组密钥";
  const savedBindingLabel = selected
    ? selected.source === "custom"
      ? "自定义生图配置"
      : savedCandidate
        ? savedCandidate.name + " · " + savedCandidate.masked_key
        : "已保存的密钥信息不可用"
    : "尚未设置已保存绑定";

  function restoreDraft() {
    setSource(selected?.source || "saved");
    setProviderId(selected?.provider_id || "");
    setEndpoint(selected?.endpoint || "https://api.osirclaw.com/v1");
    setModel(selected?.model || "");
    setKey("");
    setDirty(false);
    setChanging(false);
  }

  function close() {
    if (busy) return;
    if (dirty) {
      setConfirmClose(true);
      return;
    }
    setKey("");
    setError("");
    setEditing(false);
  }
  function edit() {
    restoreDraft();
    setError("");
    setEditing(true);
  }
  useEffect(() => {
    if (openRequest > 0 && session.data?.connected) {
      edit();
      onOpenHandled?.();
    }
  }, [openRequest, session.data?.connected, onOpenHandled]);
  async function save(event: React.FormEvent) {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const request =
        source === "saved"
          ? { source, provider_id: providerId, model: "", generation }
          : {
              source,
              endpoint: endpoint.trim(),
              model: model.trim(),
              api_key: key,
            };
      const result = await invoke<ImageConfig>("osir_image_save", { request });
      query.setQueryData(["osir-global-image", generation], result);
      setKey("");
      setDirty(false);
      setEditing(false);
      if (result.reason) toast.warning(result.reason);
      else toast.success("原生生图配置信息已保存。");
    } catch {
      setKey("");
      setError(
        "配置未确认，请重试。账户密钥需支持生图；自定义地址请提供对应密钥。",
      );
    } finally {
      setBusy(false);
    }
  }
  if (!session.data?.connected) return null;
  return (
    <Sheet
      open={editing}
      onOpenChange={(open) => {
        if (open) edit();
        else close();
      }}
    >
      <section className="osir-image-global" aria-label="原生生图配置信息">
        <div className="osir-image-compact">
          <span className="osir-image-icon">
            <Image size={22} />
          </span>
          <div className="osir-image-caption">
            <strong>原生生图配置信息</strong>
            <small>
              {config.isPending
                ? "正在读取配置…"
                : config.isError
                  ? "配置读取失败"
                  : savedCaption}
            </small>
          </div>
          <span className="osir-group-tag">
            {config.isError
              ? "读取失败"
              : selected && config.data?.status !== "unavailable"
                ? "已配置"
                : "待配置"}
          </span>
          {config.isError && (
            <button onClick={() => void config.refetch()}>重试</button>
          )}
          <SheetTrigger asChild>
            <button disabled={!desktop || config.isPending || busy}>
              配置生图
            </button>
          </SheetTrigger>
        </div>
        <SheetContent
          width={420}
          closeLabel="关闭原生生图配置"
          className="osir-drawer osir-image-drawer"
          onEscapeKeyDown={(event) => {
            if (busy) event.preventDefault();
          }}
        >
          <SheetHeader>
            <SheetTitle>原生生图配置信息</SheetTitle>
            <SheetDescription>
              Codex 全局配置 · 与聊天供应商独立
            </SheetDescription>
          </SheetHeader>
          <SheetBody>
            <h3 className="osir-binding-heading">
              {showDraft ? "待保存的选择" : "当前绑定"}
            </h3>
            {showDraft && (
              <p className="osir-binding-note">
                当前绑定：{savedBindingLabel}。下方是待保存选择；取消将还原。
              </p>
            )}
            <dl className="osir-binding-table">
              <div>
                <dt>
                  <KeyRound size={16} />
                  密钥来源
                </dt>
                <dd>
                  {summarySource === "custom"
                    ? "自定义生图密钥"
                    : "OSIRAPI 账户密钥"}
                </dd>
              </div>
              <div>
                <dt>
                  <KeyRound size={16} />
                  密钥
                </dt>
                <dd>
                  <span className="osir-image-key-summary">
                    <span>{summaryName}</span>
                    <small>{summaryKey}</small>
                  </span>
                  <button
                    onClick={() => setChanging((v) => !v)}
                    disabled={busy}
                  >
                    {changing ? "收起修改" : "更换密钥"}
                  </button>
                </dd>
              </div>
              <div>
                <dt>
                  <Layers size={16} />
                  分组
                </dt>
                <dd>
                  {groupLabel(summaryCandidate, summarySource, showDraft)}
                  <span className="osir-group-tag">只读</span>
                </dd>
              </div>
              <div>
                <dt>
                  <Image size={16} />
                  图片模型
                </dt>
                <dd>{summaryModel}</dd>
              </div>
              <div>
                <dt>接口地址</dt>
                <dd className="osir-binding-endpoint">
                  {summaryEndpoint}
                  <button
                    onClick={() => {
                      setChanging(true);
                      if (source !== "custom") {
                        setSource("custom");
                        setDirty(true);
                      }
                    }}
                    disabled={busy}
                  >
                    修改
                  </button>
                </dd>
              </div>
            </dl>
            <p className="osir-binding-note">
              <LockKeyhole size={16} />
              切换聊天模型时保持此绑定
            </p>
            <form
              hidden={!changing && !!selected}
              id="osir-image-settings"
              className="osir-form osir-image-form"
              onChange={() => setDirty(true)}
              onSubmit={(event) => void save(event)}
            >
              <label>
                密钥来源
                <select
                  value={source}
                  disabled={busy}
                  onChange={(event) =>
                    setSource(event.target.value as "saved" | "custom")
                  }
                >
                  <option value="saved">已添加的 OSIR 账户密钥</option>
                  <option value="custom">自定义密钥与接口</option>
                </select>
              </label>
              {source === "saved" ? (
                <label>
                  使用密钥
                  <select
                    value={providerId}
                    disabled={busy}
                    onChange={(event) => setProviderId(event.target.value)}
                  >
                    <option value="">选择密钥</option>
                    {config.data?.candidates.map((p) => (
                      <option value={p.provider_id} key={p.provider_id}>
                        {p.name} · {p.masked_key}
                      </option>
                    ))}
                  </select>
                </label>
              ) : (
                <>
                  <label>
                    原生生图接口地址
                    <input
                      type="url"
                      required
                      value={endpoint}
                      disabled={busy}
                      onChange={(event) => setEndpoint(event.target.value)}
                      placeholder="https://example.com/v1"
                    />
                  </label>
                  <label>
                    API 密钥
                    <input
                      type="password"
                      required
                      autoComplete="new-password"
                      value={key}
                      disabled={busy}
                      onChange={(event) => setKey(event.target.value)}
                      placeholder="输入此接口对应的密钥"
                    />
                  </label>
                  <label>
                    生图模型
                    <input
                      required
                      value={model}
                      disabled={busy}
                      onChange={(event) => setModel(event.target.value)}
                    />
                  </label>
                </>
              )}
            </form>
            {error && (
              <p role="alert" className="osir-error">
                {error}
              </p>
            )}
            <h3 className="osir-binding-heading">能力状态</h3>
            <dl className="osir-binding-table">
              <div>
                <dt>
                  <Image size={16} />
                  生成图片
                </dt>
                <dd>
                  <span className="osir-group-tag">
                    {config.isError
                      ? "读取失败"
                      : config.data?.status === "unavailable"
                        ? "暂不可用"
                        : "调用待验证"}
                  </span>
                </dd>
              </div>
              <div>
                <dt>
                  <Image size={16} />
                  多参考图编辑
                </dt>
                <dd>
                  <span className="osir-group-tag">
                    {config.isError ? "读取失败" : "待验证"}
                  </span>
                </dd>
              </div>
            </dl>
            {config.data?.reason && (
              <p className="osir-muted" role="status">
                {config.data.reason}
              </p>
            )}
            <button
              disabled={busy || config.isFetching}
              onClick={() => void config.refetch()}
            >
              {config.isFetching ? "正在读取…" : "重新读取能力"}
            </button>
          </SheetBody>
          <SheetFooter>
            <button
              type="submit"
              form="osir-image-settings"
              className="primary"
              disabled={
                busy ||
                (!changing && !!selected) ||
                (!dirty && !!selected) ||
                (source === "saved" && (!providerId || !draftCandidate))
              }
            >
              {busy ? "保存中…" : "保存配置"}
            </button>
            <button type="button" disabled={busy} onClick={close}>
              取消
            </button>
          </SheetFooter>
        </SheetContent>
        <Dialog open={confirmClose} onOpenChange={setConfirmClose}>
          <DialogContent zIndex="alert" role="alertdialog">
            <DialogHeader>
              <DialogTitle>放弃未保存的生图配置？</DialogTitle>
              <DialogDescription>
                关闭后丢弃本次修改，当前已保存的全局配置保持不变。
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <button autoFocus onClick={() => setConfirmClose(false)}>
                继续编辑
              </button>
              <button
                onClick={() => {
                  setError("");
                  restoreDraft();
                  setConfirmClose(false);
                  setEditing(false);
                }}
              >
                放弃修改
              </button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </section>
    </Sheet>
  );
}
