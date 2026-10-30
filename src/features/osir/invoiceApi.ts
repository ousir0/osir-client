import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  accountQueryRoot,
  accountSessionKey,
  useAccountSession,
  useAccountVisible,
  type AccountSession,
} from "./accountApi";

/**
 * Customer-service DTO projection from the released website invoice patch
 * at 49fde4052be736fdf30d241ac8fa1a926818f5b2 (2026-10-29).
 * See backend/internal/handler/dto/support_ticket.go and frontend/src/api/tickets.ts.
 */
export type InvoiceStatus = "pending_admin" | "pending_user" | "resolved";
export interface InvoiceRecord {
  id: number;
  ticket_no: string;
  status: string;
  invoice_amount: number;
  invoice_currency: string;
  invoice_order_count: number;
  invoice_type: string;
  invoice_service_name: string;
  user_unread: boolean;
  created_at: string;
  updated_at: string;
  resolved_at?: string | null;
}
export interface InvoiceOrder {
  id: number;
  out_trade_no: string;
  payment_type: string;
  order_type: string;
  pay_amount: number;
  currency: string;
  paid_at?: string | null;
}
export interface InvoiceEligibility {
  enabled: boolean;
  eligible: boolean;
  threshold: number;
  amount: number;
  currency: string;
  order_count: number;
  orders: InvoiceOrder[];
}
export interface InvoiceAttachment {
  id: number;
  file_name: string;
  content_type: string;
  size_bytes: number;
}
export interface InvoiceMessage {
  id: number;
  sender_role: string;
  content: string;
  created_at: string;
  attachments: InvoiceAttachment[];
}
export interface InvoiceDetail {
  ticket: InvoiceRecord;
  messages: InvoiceMessage[];
  invoice_orders: InvoiceOrder[];
}
export interface InvoicePage {
  items: InvoiceRecord[];
  total: number;
  page: number;
  page_size: number;
}
export interface InvoiceReply<T> {
  generation: number;
  data: T;
}
export interface InvoiceForm {
  title: string;
  taxNumber: string;
  email: string;
  note: string;
}
export interface InvoiceAttempt {
  state: "none" | "unknown" | "confirmed";
  ticket_id?: number | null;
}
export type InvoiceErrorCode =
  | "scope_required"
  | "unauthorized"
  | "stale_session"
  | "unavailable"
  | "rate_limited"
  | "invalid_response"
  | "invalid_input"
  | "orders_changed"
  | "not_eligible"
  | "disabled"
  | "open_limit"
  | "outcome_unknown"
  | "pdf_unavailable"
  | "save_failed";
const messages: Record<InvoiceErrorCode, string> = {
  scope_required:
    "当前授权尚未包含开票权限。请前往账户登录管理，退出账户后重新授权登录，并在浏览器中确认开票权限。",
  unauthorized: "账户授权已失效，请重新登录后查看。",
  stale_session: "账户已切换，请重新打开开票管理。",
  unavailable: "开票服务暂时不可用，请稍后手动重试。",
  rate_limited: "请求较频繁，请稍后手动重试。",
  invalid_response: "开票数据未能确认，请刷新重试。",
  invalid_input: "请检查发票抬头、纳税人识别号、邮箱和订单确认。",
  orders_changed: "可开票订单已变化，请刷新订单并重新确认。",
  not_eligible: "当前订单尚不满足网站开票条件。",
  disabled: "网站暂未开放开票申请。",
  open_limit: "待处理工单已达到网站上限，请先联系 OSIR 客服。",
  outcome_unknown:
    "提交结果尚未确认。为避免重复申请，已暂停再次提交；请核对申请记录或联系 OSIR 客服。",
  pdf_unavailable:
    "所选附件不是可验证的 PDF，或文件已不可用，请刷新记录后重试。",
  save_failed: "文件未能安全保存，请选择可写目录和未使用的 PDF 文件名。",
};
/** Never display arbitrary invoke/HTTP errors, URLs or backend body text. */
export function invoiceErrorCode(error: unknown): InvoiceErrorCode {
  const code =
    typeof error === "object" && error !== null && "code" in error
      ? (error as { code: unknown }).code
      : undefined;
  return typeof code === "string" &&
    Object.prototype.hasOwnProperty.call(messages, code)
    ? (code as InvoiceErrorCode)
    : "unavailable";
}
export const invoiceErrorText = (error: unknown) =>
  messages[invoiceErrorCode(error)];
export function validateInvoiceForm(form: InvoiceForm) {
  const errors: Partial<Record<keyof InvoiceForm | "orders", string>> = {};
  const clean = Object.fromEntries(
    Object.entries(form).map(([key, value]) => [key, value.trim()]),
  ) as unknown as InvoiceForm;
  if (
    !clean.title ||
    Array.from(clean.title).length > 120 ||
    /[\r\n\x00-\x1f\x7f]/.test(clean.title)
  )
    errors.title = "填写发票抬头，最多 120 字，使用单行文字。";
  if (
    !clean.taxNumber ||
    Array.from(clean.taxNumber).length > 40 ||
    /[\r\n\x00-\x1f\x7f]/.test(clean.taxNumber)
  )
    errors.taxNumber = "填写纳税人识别号，最多 40 字，使用单行文字。";
  if (
    clean.email.length > 255 ||
    !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(clean.email)
  )
    errors.email = "填写有效的接收邮箱。";
  if (
    Array.from(clean.note).length > 2000 ||
    /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(clean.note)
  )
    errors.note = "备注最多 2000 字，请移除不可见控制字符。";
  return { clean, errors };
}
export function invoiceStatusText(status: string) {
  return (
    (
      {
        pending_admin: "待客服处理",
        pending_user: "客服已回复",
        resolved: "工单已完成",
      } as Record<string, string>
    )[status] || "状态待确认"
  );
}
export const invoiceApi = {
  list: (generation: number, page: number, status: string) =>
    invoke<InvoiceReply<InvoicePage>>("osir_invoice_list", {
      generation,
      page,
      status,
    }),
  eligibility: (generation: number) =>
    invoke<InvoiceReply<InvoiceEligibility>>("osir_invoice_eligibility", {
      generation,
    }),
  detail: (generation: number, id: number) =>
    invoke<InvoiceReply<InvoiceDetail>>("osir_invoice_detail", {
      generation,
      id,
    }),
  attempt: (generation: number) =>
    invoke<InvoiceReply<InvoiceAttempt>>("osir_invoice_attempt", {
      generation,
    }),
  apply: (
    generation: number,
    attemptId: string,
    form: InvoiceForm,
    orderIds: number[],
  ) =>
    invoke<InvoiceReply<InvoiceDetail>>("osir_invoice_apply", {
      generation,
      attemptId,
      form,
      orderIds,
    }),
  download: (generation: number, id: number, attachmentId: number) =>
    invoke<InvoiceReply<{ saved: boolean }>>("osir_invoice_download_pdf", {
      generation,
      id,
      attachmentId,
    }),
};
/** Logout's existing account-root bridge cancels and removes all invoice PII. */
export function useInvoiceQuery<T>(
  key: readonly unknown[],
  read: (generation: number) => Promise<InvoiceReply<T>>,
  enabled = true,
) {
  const session = useAccountSession();
  const visible = useAccountVisible();
  const client = useQueryClient();
  const generation = session.data?.generation;
  const native =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  return useQuery({
    queryKey: [...accountQueryRoot, generation, "invoices", ...key],
    queryFn: async ({ signal }) => {
      const check = () => {
        const current = client.getQueryData<AccountSession>(accountSessionKey);
        if (
          signal.aborted ||
          !current?.connected ||
          current.generation !== generation
        )
          throw { code: "stale_session" };
      };
      check();
      const reply = await read(generation!);
      check();
      if (reply.generation !== generation) throw { code: "stale_session" };
      return reply;
    },
    enabled:
      native &&
      enabled &&
      visible &&
      !!session.data?.connected &&
      !session.isError,
    retry: false,
    retryOnMount: false,
    staleTime: 60_000,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
}
