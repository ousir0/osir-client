import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import {
  useQuery,
  useQueryClient,
  hashKey,
  type QueryClient,
} from "@tanstack/react-query";

export interface AccountSession {
  connected: boolean;
  recovery?:
    | "pending"
    | "restoring"
    | "unlock_required"
    | "retryable"
    | "blocked"
    | "idle";
  canCreateKeys?: boolean;
  generation: number;
  profile?: AccountProfile | null;
}
export interface LogoutReport {
  generation: number;
  localCleared: boolean;
  remoteRevoked: boolean;
}
export interface AccountProfile {
  id: number;
  email: string;
  username: string;
  balance: number;
}
export type AccountResource =
  | "profile"
  | "keys"
  | "groups"
  | "group_rates"
  | "subscriptions"
  | "usage"
  | "usage_stats"
  | "usage_snapshot"
  | "announcements";
export interface ResourceReply<T> {
  data: T;
  generation: number;
  fetched_at: string;
}
export const accountQueryRoot = ["osir-account"] as const;
export const accountSessionKey = [...accountQueryRoot, "session"] as const;
const native = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Apply native snapshots monotonically; keep the observed session query alive. */
export function applyAccountSession(client: QueryClient, next: AccountSession) {
  const current = client.getQueryData<AccountSession>(accountSessionKey);
  if (current && current.generation > next.generation) return current;
  // A verified session cannot regress to an older startup snapshot in the
  // same generation. Invalidation/logout always advances the native generation.
  if (
    current?.generation === next.generation &&
    current.connected &&
    !next.connected
  )
    return current;
  const accepted = next.connected ? next : { ...next, profile: undefined };
  client.setQueryData(accountSessionKey, accepted);
  const obsolete = {
    predicate: (query: { queryKey: readonly unknown[] }) =>
      query.queryKey[0] === accountQueryRoot[0] &&
      query.queryKey[1] !== "session" &&
      (!accepted.connected || query.queryKey[1] !== accepted.generation),
  };
  // Cancellation prevents an in-flight invoke from repopulating removed queries.
  void client.cancelQueries(obsolete);
  client.removeQueries(obsolete);
  return accepted;
}

/** Visibility gates resource reads. Native coordination remains authoritative. */
export function useAccountVisible() {
  const [visible, setVisible] = useState(
    () => document.visibilityState !== "hidden",
  );
  useEffect(() => {
    const update = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, []);
  return visible;
}
interface SessionBridge {
  observers: Set<(error: unknown) => void>;
  error: unknown;
  disposed: boolean;
  pending: boolean;
  retry: () => void;
  stop?: () => void;
}
const sessionBridges = new WeakMap<QueryClient, SessionBridge>();
function observeSession(
  client: QueryClient,
  observer: (error: unknown) => void,
) {
  let bridge = sessionBridges.get(client);
  if (!bridge) {
    bridge = {
      observers: new Set(),
      error: null,
      disposed: false,
      pending: false,
      retry: () => {},
    };
    sessionBridges.set(client, bridge);
    const active = bridge;
    active.retry = () => {
      if (active.disposed || active.pending || active.stop) return;
      active.pending = true;
      void listen<AccountSession>(
        "osir-account-session-changed",
        ({ payload }) => {
          if (active.disposed) return;
          active.error = null;
          active.observers.forEach((notify) => notify(null));
          applyAccountSession(client, payload);
        },
      )
        .then((stop) => {
          if (active.disposed) {
            stop();
            return;
          }
          active.stop = stop;
          active.error = null;
          active.observers.forEach((notify) => notify(null));
          // Close the snapshot/listener gap once for all observers, without
          // automatically retrying an already failed session read.
          if (client.getQueryState(accountSessionKey)?.status !== "error")
            void client.invalidateQueries({
              queryKey: accountSessionKey,
              exact: true,
            });
        })
        .catch((error: unknown) => {
          if (active.disposed) return;
          active.error = error;
          active.observers.forEach((notify) => notify(error));
        })
        .finally(() => {
          active.pending = false;
        });
    };
    active.retry();
  }
  bridge.observers.add(observer);
  observer(bridge.error);
  const active = bridge;
  return () => {
    active.observers.delete(observer);
    if (!active.observers.size) {
      active.disposed = true;
      active.stop?.();
      sessionBridges.delete(client);
    }
  };
}
export function useAccountSession() {
  const client = useQueryClient();
  const [eventError, setEventError] = useState<unknown>(null);
  const session = useQuery({
    queryKey: accountSessionKey,
    queryFn: async ({ signal }) => {
      const before = client.getQueryData<AccountSession>(accountSessionKey);
      const status = await invoke<AccountSession>("osir_account_session");
      if (signal.aborted) throw new Error("账户状态读取已取消");
      const current = client.getQueryData<AccountSession>(accountSessionKey);
      if (current && current !== before) return current;
      return applyAccountSession(client, status);
    },
    enabled: native(),
    retry: false,
    retryOnMount: false,
    staleTime: Infinity,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  useEffect(() => {
    if (native()) return observeSession(client, setEventError);
  }, [client]);
  return {
    ...session,
    // A disabled query is pending without fetching. Browser preview has no
    // native session to load; keep its data absent rather than invent a login.
    isPending: native() && session.isPending,
    isLoading: native() && session.isLoading,
    refetch: (options?: Parameters<typeof session.refetch>[0]) => {
      if (!native()) return Promise.resolve(session);
      sessionBridges.get(client)?.retry();
      return session.refetch(options);
    },
    retrySaved: async () => {
      const before = client.getQueryData<AccountSession>(accountSessionKey);
      if (
        !native() ||
        !before ||
        !["retryable", "unlock_required"].includes(before.recovery || "")
      )
        throw new Error("当前登录状态不能重试恢复");
      const status = await invoke<AccountSession>("osir_account_session", {
        retrySaved: true,
        generation: before.generation,
      });
      return applyAccountSession(client, status);
    },
    // A notification failure does not invalidate a verified native login.
    // Every resource command still validates its live account and generation.
    isEventError: !!eventError,
    isError: session.isError || (!!eventError && !session.data?.connected),
    error: session.error || (!session.data?.connected ? eventError : null),
  };
}
export function useAccountResource<T>(
  resource: AccountResource,
  parameters: Record<string, unknown> = {},
  enabled = true,
) {
  const client = useQueryClient();
  const session = useAccountSession();
  const visible = useAccountVisible();
  const generation = session.data?.generation;
  const queryKey = [...accountQueryRoot, generation, resource, parameters];
  const requestKey = hashKey(queryKey);
  const freshReads = useRef(new Map<string, number>());
  const query = useQuery({
    queryKey,
    queryFn: async ({ signal }) => {
      const before = client.getQueryData<AccountSession>(accountSessionKey);
      if (
        signal.aborted ||
        !before?.connected ||
        before.generation !== generation
      )
        throw new Error("账户已切换，请重新读取");
      const reply = await invoke<ResourceReply<T>>("osir_account_resource", {
        resource,
        parameters,
        generation,
        ...(freshReads.current.has(requestKey) ? { refresh: true } : {}),
      });
      const current = client.getQueryData<AccountSession>(accountSessionKey);
      if (
        signal.aborted ||
        !current?.connected ||
        reply.generation !== generation ||
        current.generation !== generation
      )
        throw new Error("账户已切换，请重新读取");
      return reply;
    },
    enabled: native() && visible && !!session.data?.connected && enabled,
    retry: false,
    retryOnMount: false,
    staleTime:
      resource === "profile"
        ? 120_000
        : resource === "usage_stats"
          ? 60_000
          : 180_000,
    gcTime: 300_000,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  return {
    ...query,
    // React Query refetch alone still hits the native TTL cache. Mark only this
    // resource+filter request as explicit refresh; never alter endpoint filters.
    refetch: async (options?: Parameters<typeof query.refetch>[0]) => {
      if (!native()) return query;
      freshReads.current.set(
        requestKey,
        (freshReads.current.get(requestKey) ?? 0) + 1,
      );
      try {
        return await query.refetch(options);
      } finally {
        const remaining = (freshReads.current.get(requestKey) ?? 1) - 1;
        if (remaining) freshReads.current.set(requestKey, remaining);
        else freshReads.current.delete(requestKey);
      }
    },
  };
}
