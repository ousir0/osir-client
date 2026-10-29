import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Bell, Check, ChevronDown, RefreshCw } from "lucide-react";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/lib/toast";
import { useAccountResource, useAccountSession } from "./accountApi";
import { connectionError } from "./errors";
import "./announcements.css";

interface Announcement {
  id: number;
  title: string;
  content: string;
  notify_mode?: string;
  starts_at?: string | null;
  ends_at?: string | null;
  read_at?: string | null;
  created_at?: string;
}
const RETURN_SYNC_STALE_MS = 60_000;
/** Text rendering intentionally avoids server HTML, script and embedded resources. */
export function OsirAnnouncements({
  variant = "panel",
}: {
  variant?: "panel" | "bell";
}) {
  const session = useAccountSession();
  if (!session.data) return null;
  return (
    <AccountAnnouncements
      key={session.data.generation}
      generation={session.data.generation}
      available={session.data.connected && !session.isError}
      variant={variant}
    />
  );
}

// The generation key discards queues, write locks and completions on account change.
function AccountAnnouncements({
  generation,
  available,
  variant,
}: {
  generation: number;
  available: boolean;
  variant: "panel" | "bell";
}) {
  const announcements = useAccountResource<Announcement[]>("announcements");
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<number | null>(null);
  const [writeError, setWriteError] = useState<{
    id: number;
    message: string;
  }>();
  const [clock, refreshClock] = useState(0);
  const [popups, setPopups] = useState(() => ({
    queue: [] as number[],
    shown: new Set<number>(),
  }));
  const writeLock = useRef(false);
  const returnSync = useRef({ pending: false, attemptedAt: -Infinity });
  const mounted = useRef(true);
  const active = useRef(available);
  active.current = available;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  // Returning to a stale window uses the same account query/native request budget.
  // Coalesce focus + visibility events, including failed reads, without polling.
  useEffect(() => {
    if (!available) return;
    const syncOnReturn = async () => {
      const now = Date.now();
      if (
        document.visibilityState === "hidden" ||
        !active.current ||
        announcements.isFetching ||
        announcements.isPending ||
        writeLock.current ||
        returnSync.current.pending ||
        now - announcements.dataUpdatedAt < RETURN_SYNC_STALE_MS ||
        now - returnSync.current.attemptedAt < RETURN_SYNC_STALE_MS
      )
        return;
      returnSync.current.pending = true;
      returnSync.current.attemptedAt = now;
      try {
        await announcements.refetch({ cancelRefetch: false });
      } catch {
        // Query state owns the error and explicit retry; avoid background toasts.
      } finally {
        returnSync.current.pending = false;
      }
    };
    window.addEventListener("focus", syncOnReturn);
    document.addEventListener("visibilitychange", syncOnReturn);
    return () => {
      window.removeEventListener("focus", syncOnReturn);
      document.removeEventListener("visibilitychange", syncOnReturn);
    };
  }, [
    available,
    announcements.dataUpdatedAt,
    announcements.isFetching,
    announcements.isPending,
    announcements.refetch,
  ]);
  // Re-evaluate schedules locally at their next boundary, without account polling.
  // Visibility/focus also catch up after background timer throttling or sleep.
  useEffect(() => {
    if (!available) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      if (timer !== undefined) clearTimeout(timer);
      const now = Date.now();
      const deadlines =
        announcements.data && announcements.data.generation === generation
          ? announcements.data.data.flatMap((item) =>
              [item.starts_at, item.ends_at]
                .filter((date): date is string => !!date)
                .map(Date.parse)
                .filter((date) => Number.isFinite(date) && date > now),
            )
          : [];
      if (deadlines.length) {
        timer = setTimeout(
          () => {
            refreshClock((value) => value + 1);
            schedule();
          },
          Math.min(Math.min(...deadlines) - now, 2_147_483_647),
        );
      }
    };
    const catchUp = () => {
      refreshClock((value) => value + 1);
      schedule();
    };
    schedule();
    window.addEventListener("focus", catchUp);
    document.addEventListener("visibilitychange", catchUp);
    return () => {
      if (timer !== undefined) clearTimeout(timer);
      window.removeEventListener("focus", catchUp);
      document.removeEventListener("visibilitychange", catchUp);
    };
  }, [announcements.data, generation, available]);
  const unique = useMemo(() => {
    const now = Date.now();
    const seen = new Set<number>();
    return (
      announcements.data?.generation === generation
        ? announcements.data.data
        : []
    ).filter((item) => {
      const start = item.starts_at ? Date.parse(item.starts_at) : null;
      const end = item.ends_at ? Date.parse(item.ends_at) : null;
      if (
        seen.has(item.id) ||
        (start !== null && (!Number.isFinite(start) || start > now)) ||
        (end !== null && (!Number.isFinite(end) || end <= now))
      )
        return false;
      seen.add(item.id);
      return true;
    });
  }, [announcements.data, generation, clock, available]);
  const eligible = useMemo(
    () =>
      unique.filter((item) => item.notify_mode === "popup" && !item.read_at),
    [unique],
  );
  useEffect(() => {
    if (!available) return;
    setPopups((previous) => {
      // Keep server order for newcomers and prune anything read, removed or expired.
      const queue = previous.queue.filter((id) =>
        eligible.some((item) => item.id === id),
      );
      if (!announcements.isError && !announcements.isPending) {
        for (const item of eligible) {
          if (!previous.shown.has(item.id) && !queue.includes(item.id))
            queue.push(item.id);
        }
      }
      let shown = previous.shown;
      if (queue.length && !shown.has(queue[0])) {
        shown = new Set(shown).add(queue[0]);
      }
      return shown === previous.shown &&
        queue.length === previous.queue.length &&
        queue.every((id, index) => id === previous.queue[index])
        ? previous
        : { queue, shown };
    });
  }, [eligible, announcements.isError, announcements.isPending, available]);
  const popup = eligible.find((item) => item.id === popups.queue[0]);
  useEffect(() => {
    if (popup) setOpen(false);
  }, [popup?.id]);
  function deferPopup() {
    if (writeLock.current || !active.current) return;
    setPopups((previous) => {
      const queue = previous.queue.slice(1);
      const shown = new Set(previous.shown);
      if (queue.length) shown.add(queue[0]);
      return { queue, shown };
    });
  }
  async function markRead(id: number) {
    if (writeLock.current || !active.current) return;
    writeLock.current = true;
    setPending(id);
    setWriteError(undefined);
    try {
      const reply = await invoke<{ generation: number }>(
        "osir_announcement_mark_read",
        { id, generation },
      );
      if (
        !mounted.current ||
        !active.current ||
        reply.generation !== generation
      )
        return;
      // The next server read determines read_at; no optimistic fabricated timestamp.
      const refreshed = await announcements.refetch({ throwOnError: true });
      if (!mounted.current || !active.current) return;
      if (refreshed.data?.generation !== generation)
        throw new Error("账户已切换，请重新读取");
      const item = refreshed.data.data.find((entry) => entry.id === id);
      if (item && !item.read_at)
        throw new Error("已读状态尚未更新，请刷新确认后重试");
      if (item?.read_at) toast.success("公告已标记为已读");
    } catch (error) {
      if (mounted.current && active.current) {
        const message = `已读状态未确认同步：${connectionError(error)}`;
        setWriteError({ id, message });
        if (variant === "bell") toast.error(message, { duration: 4500 });
      }
    } finally {
      writeLock.current = false;
      if (mounted.current) setPending(null);
    }
  }
  if (!available) return null;
  const unread = unique.filter((item) => !item.read_at).length;
  const content = (
    <section className="osir-announcements" aria-label="OSIR 公告">
      <header className="osir-announcements-header">
        <div>
          <h2>
            <Bell size={18} />
            通知公告
          </h2>
          <p>{unread ? `${unread} 条未读` : "来自 OSIRAPI 的最新消息"}</p>
        </div>
        <button
          className="osir-announcement-icon"
          aria-label="刷新公告"
          title="刷新公告"
          disabled={announcements.isFetching || pending !== null}
          onClick={() => void announcements.refetch()}
        >
          <RefreshCw
            size={16}
            className={announcements.isFetching ? "animate-spin" : ""}
          />
        </button>
      </header>
      <div className="osir-announcements-list">
        {writeError && <p role="alert">{writeError.message}</p>}
        {announcements.isPending ? (
          <p role="status">正在读取公告…</p>
        ) : announcements.isError ? (
          <p role="alert" className="osir-announcement-empty">
            {connectionError(announcements.error)}
            <br />
            请点击右上角刷新重试。
          </p>
        ) : unique.length ? (
          unique.map((item) => (
            <details key={item.id} className="osir-announcement-item">
              <summary>
                <span>
                  {item.title}
                  {!item.read_at ? " · 未读" : ""}
                </span>
                <ChevronDown size={15} aria-hidden="true" />
              </summary>
              {item.starts_at && (
                <time>
                  {new Date(item.starts_at).toLocaleDateString("zh-CN")}
                </time>
              )}
              <p className="osir-announcement-content">{item.content}</p>
              {!item.read_at && (
                <button
                  className="osir-announcement-read"
                  disabled={pending !== null}
                  onClick={() => void markRead(item.id)}
                >
                  <Check size={14} aria-hidden="true" />
                  {pending === item.id ? "正在同步已读…" : "标记已读"}
                </button>
              )}
            </details>
          ))
        ) : (
          <div className="osir-announcement-empty">
            <Bell size={26} />
            <p>暂无当前有效公告。</p>
          </div>
        )}
      </div>
      <p className="osir-announcements-footer">
        展开查看内容，标记已读后同步到 OSIRAPI。
      </p>
    </section>
  );
  return (
    <>
      <Dialog
        open={!!popup}
        onOpenChange={(next) => {
          if (!next) deferPopup();
        }}
      >
        {popup && (
          <DialogContent
            className="osir-drawer osir-announcement-popup"
            zIndex="top"
            onEscapeKeyDown={(event) => {
              if (writeLock.current) event.preventDefault();
            }}
          >
            <DialogHeader className="osir-announcement-popup-header">
              <DialogDescription>
                <Bell size={16} aria-hidden="true" /> 通知公告 · 未读
              </DialogDescription>
              <DialogTitle>{popup.title}</DialogTitle>
              {popup.created_at &&
                Number.isFinite(Date.parse(popup.created_at)) && (
                  <time dateTime={popup.created_at}>
                    {new Date(popup.created_at).toLocaleString("zh-CN")}
                  </time>
                )}
            </DialogHeader>
            <div key={popup.id} className="osir-announcement-popup-body">
              <p className="osir-announcement-content">{popup.content}</p>
            </div>
            {writeError?.id === popup.id && (
              <p role="alert" className="osir-announcement-popup-error">
                {writeError.message}
              </p>
            )}
            <DialogFooter className="osir-announcement-popup-footer">
              <p>
                {popups.queue.length > 1
                  ? `还有 ${popups.queue.length - 1} 条公告`
                  : "稍后可从铃铛查看"}
              </p>
              <button disabled={pending !== null} onClick={deferPopup}>
                稍后阅读
              </button>
              <button
                className="primary"
                disabled={pending !== null}
                onClick={() => void markRead(popup.id)}
              >
                <Check size={14} aria-hidden="true" />
                {pending === popup.id ? "正在同步已读…" : "标记已读"}
              </button>
            </DialogFooter>
          </DialogContent>
        )}
      </Dialog>
      {variant === "panel" ? (
        content
      ) : (
        <Popover
          open={open}
          onOpenChange={(next) => {
            setOpen(next);
            if (
              next &&
              !announcements.isFetching &&
              !announcements.isError &&
              Date.now() - announcements.dataUpdatedAt >= 180_000
            )
              void announcements.refetch();
          }}
        >
          <PopoverTrigger asChild>
            <button
              className="osir-announcement-bell"
              aria-label={unread ? `通知公告，${unread} 条未读` : "通知公告"}
              title="通知公告"
            >
              <Bell size={19} aria-hidden="true" />
              {unread > 0 && <span>{unread > 99 ? "99+" : unread}</span>}
            </button>
          </PopoverTrigger>
          <PopoverContent
            align="end"
            sideOffset={8}
            className="osir-drawer osir-announcements-popover"
            aria-label="通知公告"
          >
            {content}
          </PopoverContent>
        </Popover>
      )}
    </>
  );
}
