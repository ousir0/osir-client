import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Download, FileText, Plus, RefreshCw } from "lucide-react";
import {
  Sheet,
  SheetBody,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useUnsavedChangesTracker } from "@/lib/unsavedChanges";
import { toast } from "@/lib/toast";
import {
  accountQueryRoot,
  accountSessionKey,
  useAccountSession,
  type AccountSession,
} from "./accountApi";
import {
  invoiceApi,
  invoiceErrorCode,
  invoiceErrorText,
  invoiceStatusText,
  useInvoiceQuery,
  validateInvoiceForm,
  type InvoiceDetail,
  type InvoiceForm,
  type InvoiceOrder,
  type InvoiceErrorCode,
} from "./invoiceApi";
import "./invoices.css";

const emptyForm: InvoiceForm = {
  title: "",
  taxNumber: "",
  email: "",
  note: "",
};
const money = (amount: number, currency: string) =>
  currency +
  " " +
  amount.toLocaleString("zh-CN", {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
const date = (value?: string | null) => {
  if (!value || !Number.isFinite(Date.parse(value))) return "—";
  return new Date(value).toLocaleString("zh-CN", { hour12: false });
};

export interface OsirInvoicesProps {
  /** Host must honor this while either drawer is open, including native saving. */
  onNavigationBlockedChange?: (blocked: boolean) => void;
  /** Navigate only; the account panel owns explicit logout and browser consent. */
  onOpenAccount?: () => void;
}
/** Parent integration: named export; account state is shared. */
export function OsirInvoices({
  onNavigationBlockedChange,
  onOpenAccount,
}: OsirInvoicesProps = {}) {
  const session = useAccountSession();
  if (session.isPending) return <p role="status">正在读取账户…</p>;
  if (session.isError || !session.data?.connected)
    return (
      <section className="osir-invoices">
        <h1>开票管理</h1>
        <p>请先登录 OSIR 账户后查看开票记录。</p>
      </section>
    );
  return (
    <InvoiceWorkspace
      key={session.data.generation}
      generation={session.data.generation}
      onNavigationBlockedChange={onNavigationBlockedChange}
      onOpenAccount={onOpenAccount}
    />
  );
}

function InvoiceAuthorizationAction({
  error,
  onOpenAccount,
  disabled = false,
}: {
  error: unknown;
  onOpenAccount?: () => void;
  disabled?: boolean;
}) {
  if (invoiceErrorCode(error) !== "scope_required" || !onOpenAccount)
    return null;
  return (
    <button type="button" disabled={disabled} onClick={onOpenAccount}>
      前往账户登录管理
    </button>
  );
}

function OrderList({ orders }: { orders: InvoiceOrder[] }) {
  return (
    <ul className="osir-invoice-orders">
      {orders.map((order) => (
        <li key={order.id}>
          <div>
            <strong>{order.out_trade_no}</strong>
            <small>
              {order.order_type === "subscription"
                ? "订阅购买"
                : order.order_type === "balance"
                  ? "余额充值"
                  : "付款订单"}{" "}
              · {date(order.paid_at)}
            </small>
          </div>
          <span>{money(order.pay_amount, order.currency)}</span>
        </li>
      ))}
    </ul>
  );
}

function InvoiceWorkspace({
  generation,
  onNavigationBlockedChange,
  onOpenAccount,
}: { generation: number } & OsirInvoicesProps) {
  const client = useQueryClient();
  const [page, setPage] = useState(1);
  const [status, setStatus] = useState("");
  const [detailId, setDetailId] = useState<number | null>(null);
  const [applicationOpen, setApplicationOpen] = useState(false);
  useEffect(() => {
    onNavigationBlockedChange?.(applicationOpen || detailId !== null);
  }, [onNavigationBlockedChange, applicationOpen, detailId]);
  useEffect(
    () => () => {
      onNavigationBlockedChange?.(false);
    },
    [onNavigationBlockedChange],
  );
  const list = useInvoiceQuery(["list", page, status], (g) =>
    invoiceApi.list(g, page, status),
  );
  const current = () => {
    const session = client.getQueryData<AccountSession>(accountSessionKey);
    return session?.connected && session.generation === generation;
  };
  return (
    <section className="osir-invoices" aria-label="开票管理">
      <header className="osir-invoice-header">
        <div>
          <h1>
            <FileText size={20} />
            开票管理
          </h1>
          <p>查看 OSIR 客服开票申请、处理进度与交付附件。</p>
        </div>
        <div className="osir-invoice-actions">
          <button
            type="button"
            disabled={list.isFetching}
            onClick={() => void list.refetch()}
            aria-label="刷新开票记录"
          >
            <RefreshCw size={14} />
            刷新
          </button>
          <InvoiceApplication
            generation={generation}
            open={applicationOpen}
            setOpen={setApplicationOpen}
            disabled={list.isError}
            onOpenAccount={onOpenAccount}
            onCreated={(detail, recovered) => {
              if (!current()) return;
              toast.success(
                recovered
                  ? "已找到对应申请，请核对客服记录。"
                  : "申请已提交，等待客服处理。",
              );
              setDetailId(detail.ticket.id);
              setPage(1);
              void client.invalidateQueries({
                queryKey: [...accountQueryRoot, generation, "invoices"],
              });
            }}
          />
        </div>
      </header>
      <div className="osir-invoice-filter">
        <label>
          处理状态
          <select
            value={status}
            onChange={(event) => {
              setStatus(event.target.value);
              setPage(1);
            }}
          >
            <option value="">全部状态</option>
            <option value="pending_admin">待客服处理</option>
            <option value="pending_user">客服已回复</option>
            <option value="resolved">工单已完成</option>
          </select>
        </label>
        <small>工单完成不代表发票已交付，请核对客服回复。</small>
      </div>
      {list.isPending ? (
        <div className="osir-invoice-empty" role="status">
          正在读取开票记录…
        </div>
      ) : list.isError ? (
        <div className="osir-invoice-empty" role="alert">
          <p>{invoiceErrorText(list.error)}</p>
          <InvoiceAuthorizationAction
            error={list.error}
            onOpenAccount={onOpenAccount}
          />
          <button onClick={() => void list.refetch()}>重新读取</button>
        </div>
      ) : !list.data?.data.items.length ? (
        <div className="osir-invoice-empty">
          <FileText size={28} />
          <h2>暂无开票申请</h2>
          <p>符合条件的实际付款订单可通过右上角申请开票。</p>
        </div>
      ) : (
        <>
          <div className="osir-invoice-table-wrap">
            <table className="osir-invoice-table">
              <thead>
                <tr>
                  <th>申请单号</th>
                  <th>申请金额</th>
                  <th>进度</th>
                  <th>最近更新</th>
                  <th>
                    <span className="sr-only">操作</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {list.data.data.items.map((record) => (
                  <tr key={record.id}>
                    <td>
                      <strong>{record.ticket_no}</strong>
                      <small>
                        {record.invoice_order_count} 笔订单 ·{" "}
                        {date(record.created_at)}
                      </small>
                    </td>
                    <td>
                      {money(record.invoice_amount, record.invoice_currency)}
                    </td>
                    <td>
                      <span
                        className="osir-invoice-status"
                        data-status={record.status}
                      >
                        {invoiceStatusText(record.status)}
                      </span>
                      {record.user_unread && <small>有新回复</small>}
                    </td>
                    <td>{date(record.updated_at)}</td>
                    <td>
                      <button
                        onClick={() => setDetailId(record.id)}
                        aria-label={"查看申请 " + record.ticket_no}
                      >
                        查看
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <footer className="osir-invoice-pagination">
            <small>
              共 {list.data.data.total} 条 · 第 {page} 页
            </small>
            <div>
              <button
                disabled={page === 1 || list.isFetching}
                onClick={() => setPage((value) => value - 1)}
              >
                上一页
              </button>
              <button
                disabled={
                  page * list.data.data.page_size >= list.data.data.total ||
                  list.isFetching
                }
                onClick={() => setPage((value) => value + 1)}
              >
                下一页
              </button>
            </div>
          </footer>
        </>
      )}
      <InvoiceDetails
        generation={generation}
        id={detailId}
        onClose={() => setDetailId(null)}
        onOpenAccount={onOpenAccount}
      />
    </section>
  );
}

function InvoiceApplication({
  generation,
  open,
  setOpen,
  disabled,
  onCreated,
  onOpenAccount,
}: {
  generation: number;
  open: boolean;
  setOpen: (open: boolean) => void;
  disabled: boolean;
  onCreated: (detail: InvoiceDetail, recovered?: boolean) => void;
  onOpenAccount?: () => void;
}) {
  const client = useQueryClient();
  const [form, setForm] = useState<InvoiceForm>(emptyForm);
  const [selected, setSelected] = useState(false);
  const [errors, setErrors] = useState<
    ReturnType<typeof validateInvoiceForm>["errors"]
  >({});
  const [error, setError] = useState<InvoiceErrorCode | "">("");
  const [unknown, setUnknown] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  const [confirmSubmit, setConfirmSubmit] = useState(false);
  const lock = useRef(false);
  const attemptId = useRef<string | null>(null);
  const accountAfterDiscard = useRef(false);
  const eligibility = useInvoiceQuery(
    ["eligibility"],
    invoiceApi.eligibility,
    open,
  );
  const attempt = useInvoiceQuery(["attempt"], invoiceApi.attempt, open);
  const eligible = eligibility.data?.data;
  const dirty = selected || Object.values(form).some((value) => !!value);
  const unsaved = useUnsavedChangesTracker(open && (dirty || busy));
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    setSelected(false);
  }, [eligibility.dataUpdatedAt]);
  useEffect(() => {
    if (!open || !dirty) return;
    const prevent = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", prevent);
    return () => window.removeEventListener("beforeunload", prevent);
  }, [open, dirty]);
  const current = () => {
    const session = client.getQueryData<AccountSession>(accountSessionKey);
    return (
      alive.current && session?.connected && session.generation === generation
    );
  };
  const blocked = unknown || attempt.data?.data.state === "unknown";
  const close = (next: boolean, account = false) => {
    if (next) {
      setOpen(true);
      return;
    }
    if (lock.current) return;
    accountAfterDiscard.current = account;
    if (dirty) setConfirmClose(true);
    else {
      setOpen(false);
      if (account) onOpenAccount?.();
    }
  };
  const discard = () => {
    setForm(emptyForm);
    setSelected(false);
    setConfirmClose(false);
    setOpen(false);
    setError("");
    setErrors({});
    attemptId.current = null;
    if (accountAfterDiscard.current) onOpenAccount?.();
    accountAfterDiscard.current = false;
  };
  async function submit() {
    if (
      lock.current ||
      !current() ||
      blocked ||
      !selected ||
      !eligible?.enabled ||
      !eligible.eligible ||
      attempt.isError ||
      !attempt.data
    )
      return;
    const validation = validateInvoiceForm(form);
    setErrors(validation.errors);
    if (Object.keys(validation.errors).length) {
      setConfirmSubmit(false);
      return;
    }
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      attemptId.current ||= crypto.randomUUID();
      const reply = await invoiceApi.apply(
        generation,
        attemptId.current,
        validation.clean,
        eligible.orders.map((order) => order.id),
      );
      if (!current() || reply.generation !== generation) return;
      setForm(emptyForm);
      setSelected(false);
      setOpen(false);
      setConfirmSubmit(false);
      attemptId.current = null;
      onCreated(reply.data);
    } catch (cause) {
      if (!current()) return;
      const code = invoiceErrorCode(cause);
      // IPC failures may lose a successful POST response. Never make those retryable.
      const definitiveRejections = [
        "scope_required",
        "unauthorized",
        "rate_limited",
        "invalid_input",
        "disabled",
        "not_eligible",
        "orders_changed",
        "open_limit",
        "unavailable",
      ];
      const uncertain = !(
        typeof cause === "object" &&
        cause !== null &&
        "code" in cause &&
        cause.code === code &&
        definitiveRejections.includes(code)
      );
      setUnknown(uncertain);
      setError(uncertain ? "outcome_unknown" : code);
      setConfirmSubmit(false);
      if (!uncertain) attemptId.current = null;
      if (code === "orders_changed" || code === "not_eligible") {
        setSelected(false);
        void eligibility.refetch();
      }
    } finally {
      lock.current = false;
      if (current()) setBusy(false);
    }
  }
  const field = (
    name: keyof InvoiceForm,
    label: string,
    max: number,
    type = "text",
  ) => (
    <label className="osir-invoice-field" htmlFor={"invoice-" + name}>
      {label}
      <input
        id={"invoice-" + name}
        type={type}
        required
        maxLength={max}
        value={form[name]}
        disabled={busy || blocked}
        aria-invalid={!!errors[name]}
        aria-describedby={errors[name] ? "invoice-error-" + name : undefined}
        onChange={(event) => {
          setForm((value) => ({ ...value, [name]: event.target.value }));
          setErrors((value) => ({ ...value, [name]: undefined }));
        }}
      />
      {errors[name] && (
        <span role="alert" id={"invoice-error-" + name}>
          {errors[name]}
        </span>
      )}
    </label>
  );
  return (
    <>
      <Sheet open={open} onOpenChange={close}>
        <SheetTrigger asChild>
          <button className="osir-invoice-primary" disabled={disabled}>
            <Plus size={14} />
            申请开票
          </button>
        </SheetTrigger>
        <SheetContent
          width={420}
          closeLabel="关闭开票申请"
          className="osir-drawer osir-invoice-drawer"
        >
          <SheetHeader>
            <SheetTitle>申请开票</SheetTitle>
            <SheetDescription>
              提交至 OSIR 客服，与网站开票记录同步。
            </SheetDescription>
          </SheetHeader>
          <SheetBody>
            <form
              id="osir-invoice-form"
              noValidate
              {...unsaved}
              onSubmit={(event) => {
                event.preventDefault();
                const result = validateInvoiceForm(form);
                if (!selected)
                  result.errors.orders = "请确认选择全部可开票订单。";
                setErrors(result.errors);
                if (!Object.keys(result.errors).length) setConfirmSubmit(true);
              }}
            >
              {eligibility.isPending || attempt.isPending ? (
                <p role="status">正在核对订单与上次提交结果…</p>
              ) : eligibility.isError || attempt.isError ? (
                <div role="alert">
                  <p>{invoiceErrorText(eligibility.error || attempt.error)}</p>
                  <InvoiceAuthorizationAction
                    error={eligibility.error || attempt.error}
                    onOpenAccount={
                      onOpenAccount ? () => close(false, true) : undefined
                    }
                    disabled={busy}
                  />
                  <button
                    type="button"
                    onClick={() => {
                      void eligibility.refetch();
                      void attempt.refetch();
                    }}
                  >
                    重新核对
                  </button>
                </div>
              ) : (
                <>
                  {blocked && (
                    <div className="osir-invoice-warning" role="alert">
                      <p>{invoiceErrorText({ code: "outcome_unknown" })}</p>
                      <button
                        type="button"
                        disabled={attempt.isFetching}
                        onClick={async () => {
                          if (lock.current) return;
                          lock.current = true;
                          setBusy(true);
                          try {
                            const result = await attempt.refetch({
                              throwOnError: true,
                            });
                            if (
                              !current() ||
                              result.data?.data.state !== "confirmed" ||
                              !result.data.data.ticket_id
                            )
                              return;
                            const found = await invoiceApi.detail(
                              generation,
                              result.data.data.ticket_id,
                            );
                            if (!current() || found.generation !== generation)
                              return;
                            setUnknown(false);
                            setError("");
                            setForm(emptyForm);
                            setSelected(false);
                            setOpen(false);
                            onCreated(found.data, true);
                          } catch (cause) {
                            if (current()) setError(invoiceErrorCode(cause));
                          } finally {
                            lock.current = false;
                            if (current()) setBusy(false);
                          }
                        }}
                      >
                        核对提交结果
                      </button>
                    </div>
                  )}
                  {eligible && (
                    <>
                      <div className="osir-invoice-amount">
                        <small>当前可开票金额</small>
                        <strong>
                          {money(eligible.amount, eligible.currency)}
                        </strong>
                        <span>{eligible.order_count} 笔订单</span>
                      </div>
                      {!eligible.enabled ? (
                        <p className="osir-invoice-warning">
                          网站暂未开放开票申请。
                        </p>
                      ) : !eligible.eligible ? (
                        <p className="osir-invoice-warning">
                          网站要求可开票金额超过{" "}
                          {money(eligible.threshold, eligible.currency)}
                          ，当前尚不满足条件。
                        </p>
                      ) : null}
                      <label className="osir-invoice-selection">
                        <input
                          type="checkbox"
                          checked={selected}
                          disabled={
                            busy ||
                            blocked ||
                            !eligible.enabled ||
                            !eligible.orders.length
                          }
                          onChange={(event) =>
                            setSelected(event.target.checked)
                          }
                        />
                        选择全部 {eligible.order_count} 笔可开票订单
                      </label>
                      <p className="osir-invoice-hint">
                        网站每次申请锁定提交时全部符合条件的订单，暂不支持逐笔拆选；提交前会再次核对订单。
                      </p>
                      {errors.orders && <p role="alert">{errors.orders}</p>}
                      <details>
                        <summary>查看可开票订单</summary>
                        <OrderList orders={eligible.orders} />
                      </details>
                    </>
                  )}
                  <div className="osir-invoice-fields">
                    {field("title", "发票抬头 *", 120)}
                    {field("taxNumber", "纳税人识别号（税号） *", 40)}
                    {field("email", "接收邮箱 *", 255, "email")}
                    <label
                      className="osir-invoice-field"
                      htmlFor="invoice-note"
                    >
                      备注（选填）
                      <textarea
                        id="invoice-note"
                        maxLength={2000}
                        rows={3}
                        value={form.note}
                        disabled={busy || blocked}
                        aria-invalid={!!errors.note}
                        onChange={(event) =>
                          setForm((value) => ({
                            ...value,
                            note: event.target.value,
                          }))
                        }
                      />
                      {errors.note && <span role="alert">{errors.note}</span>}
                    </label>
                  </div>
                  <p className="osir-invoice-hint">
                    开票类型与服务项目按网站客服规则处理；申请成功后以客服回复为准。
                  </p>
                </>
              )}
              {error && !(blocked && error === "outcome_unknown") && (
                <div role="alert" className="osir-invoice-warning">
                  <p>{invoiceErrorText({ code: error })}</p>
                  <InvoiceAuthorizationAction
                    error={{ code: error }}
                    onOpenAccount={
                      onOpenAccount ? () => close(false, true) : undefined
                    }
                    disabled={busy}
                  />
                </div>
              )}
            </form>
          </SheetBody>
          <SheetFooter>
            <button type="button" disabled={busy} onClick={() => close(false)}>
              取消
            </button>
            <button
              className="osir-invoice-primary"
              type="submit"
              form="osir-invoice-form"
              disabled={
                busy ||
                blocked ||
                eligibility.isFetching ||
                attempt.isFetching ||
                eligibility.isError ||
                attempt.isError ||
                !eligible?.eligible ||
                !eligible.enabled
              }
            >
              {busy ? "正在提交…" : "核对并申请"}
            </button>
          </SheetFooter>
        </SheetContent>
      </Sheet>
      <Dialog
        open={confirmClose}
        onOpenChange={(next) => {
          setConfirmClose(next);
          if (!next) accountAfterDiscard.current = false;
        }}
      >
        <DialogContent zIndex="alert" className="osir-invoice-dialog">
          <DialogHeader>
            <DialogTitle>放弃开票草稿？</DialogTitle>
            <DialogDescription>已填写的开票信息尚未提交。</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <button
              onClick={() => {
                accountAfterDiscard.current = false;
                setConfirmClose(false);
              }}
            >
              继续填写
            </button>
            <button onClick={discard}>放弃草稿</button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={confirmSubmit}
        onOpenChange={(next) => {
          if (!busy) setConfirmSubmit(next);
        }}
      >
        <DialogContent
          zIndex="alert"
          className="osir-invoice-dialog osir-invoice-submit-dialog"
          aria-busy={busy}
        >
          <DialogHeader>
            <DialogTitle>确认提交开票申请</DialogTitle>
            <DialogDescription>
              请核对开票信息，确认后将提交至 OSIR 客服。
            </DialogDescription>
          </DialogHeader>
          <div className="osir-invoice-confirm-body">
            <section
              className="osir-invoice-confirm-total"
              aria-label="本次申请金额"
            >
              <div>
                <span>本次申请金额</span>
                <strong>
                  {eligible && money(eligible.amount, eligible.currency)}
                </strong>
              </div>
              <span>{eligible?.order_count} 笔订单</span>
            </section>
            <dl className="osir-invoice-confirm">
              <div>
                <dt>发票抬头</dt>
                <dd>{form.title.trim()}</dd>
              </div>
              <div>
                <dt>纳税人识别号</dt>
                <dd>{form.taxNumber.trim()}</dd>
              </div>
              <div>
                <dt>接收邮箱</dt>
                <dd>{form.email.trim()}</dd>
              </div>
              {form.note.trim() && (
                <div>
                  <dt>备注</dt>
                  <dd className="osir-invoice-confirm-note">
                    {form.note.trim()}
                  </dd>
                </div>
              )}
            </dl>
            <p className="osir-invoice-confirm-hint">
              将包含提交时全部符合条件的订单，处理进度与交付结果请在开票记录中查看。
            </p>
          </div>
          <DialogFooter>
            <button
              type="button"
              disabled={busy}
              onClick={() => setConfirmSubmit(false)}
            >
              返回检查
            </button>
            <button
              className="osir-invoice-primary"
              type="button"
              disabled={busy}
              onClick={() => void submit()}
            >
              {busy ? "正在提交…" : "确认提交"}
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function InvoiceDetails({
  generation,
  id,
  onClose,
  onOpenAccount,
}: {
  generation: number;
  id: number | null;
  onClose: () => void;
  onOpenAccount?: () => void;
}) {
  const client = useQueryClient();
  const detail = useInvoiceQuery(
    ["detail", id],
    (g) => invoiceApi.detail(g, id!),
    id !== null,
  );
  const [saving, setSaving] = useState<number | null>(null);
  const [error, setError] = useState<InvoiceErrorCode | "">("");
  const lock = useRef(false);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    setError("");
  }, [id]);
  const data = detail.data?.data;
  async function download(attachmentId: number) {
    if (lock.current || !id) return;
    lock.current = true;
    setSaving(attachmentId);
    setError("");
    const current = () =>
      alive.current &&
      client.getQueryData<AccountSession>(accountSessionKey)?.connected &&
      client.getQueryData<AccountSession>(accountSessionKey)?.generation ===
        generation;
    try {
      const reply = await invoiceApi.download(generation, id, attachmentId);
      if (current() && reply.generation === generation) {
        if (reply.data.saved) toast.success("PDF 已保存。");
        else toast.info("已取消保存。");
      }
    } catch (cause) {
      if (current()) setError(invoiceErrorCode(cause));
    } finally {
      lock.current = false;
      if (current()) setSaving(null);
    }
  }
  const openAccount = onOpenAccount
    ? () => {
        if (lock.current) return;
        onClose();
        onOpenAccount();
      }
    : undefined;
  return (
    <Sheet
      open={id !== null}
      onOpenChange={(open) => {
        if (!open && !lock.current) onClose();
      }}
    >
      <SheetContent
        width={420}
        closeLabel="关闭开票详情"
        className="osir-drawer osir-invoice-drawer"
      >
        <SheetHeader>
          <SheetTitle>开票申请详情</SheetTitle>
          <SheetDescription>
            {data?.ticket.ticket_no || "正在读取客服工单"}
          </SheetDescription>
        </SheetHeader>
        <SheetBody>
          {detail.isPending ? (
            <p role="status">正在读取详情…</p>
          ) : detail.isError ? (
            <div role="alert">
              <p>{invoiceErrorText(detail.error)}</p>
              <InvoiceAuthorizationAction
                error={detail.error}
                onOpenAccount={openAccount}
                disabled={saving !== null}
              />
              <button onClick={() => void detail.refetch()}>重新读取</button>
            </div>
          ) : (
            data && (
              <>
                <div className="osir-invoice-amount">
                  <small>{invoiceStatusText(data.ticket.status)}</small>
                  <strong>
                    {money(
                      data.ticket.invoice_amount,
                      data.ticket.invoice_currency,
                    )}
                  </strong>
                  <span>{data.ticket.invoice_order_count} 笔订单</span>
                </div>
                <dl className="osir-invoice-meta">
                  <dt>申请时间</dt>
                  <dd>{date(data.ticket.created_at)}</dd>
                  <dt>最近更新</dt>
                  <dd>{date(data.ticket.updated_at)}</dd>
                  <dt>开票类型</dt>
                  <dd>
                    {data.ticket.invoice_type === "vat_ordinary"
                      ? "增值税普通发票"
                      : data.ticket.invoice_type || "待客服确认"}
                  </dd>
                  <dt>服务项目</dt>
                  <dd>
                    {data.ticket.invoice_service_name === "technical_service"
                      ? "技术服务费"
                      : data.ticket.invoice_service_name || "待客服确认"}
                  </dd>
                </dl>
                <details>
                  <summary>查看关联订单</summary>
                  <OrderList orders={data.invoice_orders} />
                </details>
                <h3>处理进度与客服回复</h3>
                <ol className="osir-invoice-timeline">
                  {data.messages.map((message) => (
                    <li key={message.id}>
                      <header>
                        <strong>
                          {message.sender_role === "admin"
                            ? "OSIR 客服"
                            : "申请人"}
                        </strong>
                        <time>{date(message.created_at)}</time>
                      </header>
                      <p>{message.content}</p>
                      {message.attachments.map((attachment) => (
                        <div
                          className="osir-invoice-attachment"
                          key={attachment.id}
                        >
                          <span>{attachment.file_name}</span>
                          {attachment.content_type.toLowerCase() ===
                          "application/pdf" ? (
                            <button
                              disabled={saving !== null}
                              onClick={() => void download(attachment.id)}
                            >
                              <Download size={13} />
                              {saving === attachment.id
                                ? "正在保存…"
                                : "保存 PDF"}
                            </button>
                          ) : (
                            <small>客服图片附件，请在网站查看</small>
                          )}
                        </div>
                      ))}
                    </li>
                  ))}
                </ol>
                {!data.messages.some((message) =>
                  message.attachments.some(
                    (item) =>
                      item.content_type.toLowerCase() === "application/pdf",
                  ),
                ) && (
                  <p className="osir-invoice-hint">
                    该工单尚无 PDF
                    附件。请核对客服说明或接收邮箱，工单完成不等于发票已交付。
                  </p>
                )}
              </>
            )
          )}
          {error && (
            <div role="alert" className="osir-invoice-warning">
              <p>{invoiceErrorText({ code: error })}</p>
              <InvoiceAuthorizationAction
                error={{ code: error }}
                onOpenAccount={openAccount}
                disabled={saving !== null}
              />
            </div>
          )}
        </SheetBody>
        <SheetFooter>
          <button
            disabled={saving !== null}
            onClick={() => {
              if (!lock.current) onClose();
            }}
          >
            关闭
          </button>
          <button
            disabled={detail.isFetching || saving !== null}
            onClick={() => void detail.refetch()}
          >
            <RefreshCw size={14} />
            刷新进度
          </button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
