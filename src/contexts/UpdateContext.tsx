import React, {
  createContext,
  useContext,
  useState,
  useEffect,
  useCallback,
  useRef,
} from "react";
import type { UpdateInfo } from "../lib/updater";
import { checkForUpdate } from "../lib/updater";
import { extractErrorMessage } from "../utils/errorUtils";
import {
  getUpdateDistribution,
  type UpdateDistribution,
} from "@/lib/updateDistribution";

interface UpdateContextValue {
  distribution: UpdateDistribution | null;
  // 更新状态
  hasUpdate: boolean;
  updateInfo: UpdateInfo | null;
  isChecking: boolean;
  error: string | null;

  // 提示状态
  isDismissed: boolean;
  dismissUpdate: () => void;

  // 操作方法
  checkUpdate: () => Promise<boolean>;
  resetDismiss: () => void;
}

const UpdateContext = createContext<UpdateContextValue | undefined>(undefined);

const LEGACY_DISMISSED_KEY = "dismissedUpdateVersion";
const dismissalKey = (kind: UpdateDistribution["kind"]) =>
  kind === "osir"
    ? "osir_client:update:dismissedVersion"
    : "ccswitch:update:dismissedVersion";

function readDismissedVersion(kind: UpdateDistribution["kind"]): string | null {
  if (kind === "unavailable") return null;
  try {
    const key = dismissalKey(kind);
    const current = localStorage.getItem(key);
    if (current || kind === "osir") return current;
    // Only the upstream product owns this legacy dismissal. Copying it to
    // OSIR could hide an unrelated OSIR release with the same version number.
    const legacy = localStorage.getItem(LEGACY_DISMISSED_KEY);
    if (legacy) {
      localStorage.setItem(key, legacy);
      localStorage.removeItem(LEGACY_DISMISSED_KEY);
    }
    return legacy;
  } catch {
    return null;
  }
}

export function UpdateProvider({ children }: { children: React.ReactNode }) {
  const [hasUpdate, setHasUpdate] = useState(false);
  const [distribution, setDistribution] = useState<UpdateDistribution | null>(
    null,
  );
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [isChecking, setIsChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isDismissed, setIsDismissed] = useState(false);

  const isCheckingRef = useRef(false);

  const runCheck = useCallback(async (automatic: boolean) => {
    if (isCheckingRef.current) return false;
    isCheckingRef.current = true;
    setIsChecking(true);
    setError(null);

    try {
      const capability = await getUpdateDistribution();
      setDistribution(capability);
      if (!capability.updatesEnabled) {
        setHasUpdate(false);
        setUpdateInfo(null);
        setIsDismissed(false);
        if (automatic) return false;
        throw new Error(capability.reason ?? "更新渠道未配置");
      }
      const result = await checkForUpdate({ timeout: 30000 });

      if (result.status === "available") {
        setHasUpdate(true);
        setUpdateInfo(result.info);

        // 检查是否已经关闭过这个版本的提醒
        setIsDismissed(
          readDismissedVersion(capability.kind) ===
            result.info.availableVersion,
        );
        return true; // 有更新
      } else {
        setHasUpdate(false);
        setUpdateInfo(null);
        setIsDismissed(false);
        return false; // 已是最新
      }
    } catch (err) {
      setError(extractErrorMessage(err) || "检查更新失败");
      setHasUpdate(false);
      throw err; // 抛出错误让调用方处理
    } finally {
      setIsChecking(false);
      isCheckingRef.current = false;
    }
  }, []);

  const checkUpdate = useCallback(() => runCheck(false), [runCheck]);

  const dismissUpdate = useCallback(() => {
    setIsDismissed(true);
    if (
      updateInfo?.availableVersion &&
      distribution?.kind !== "unavailable" &&
      distribution
    ) {
      try {
        localStorage.setItem(
          dismissalKey(distribution.kind),
          updateInfo.availableVersion,
        );
        if (distribution.kind === "ccswitch")
          localStorage.removeItem(LEGACY_DISMISSED_KEY);
      } catch {
        // Keep the dismissal for this window even when storage is unavailable.
      }
    }
  }, [updateInfo?.availableVersion, distribution]);

  const resetDismiss = useCallback(() => {
    setIsDismissed(false);
    if (!distribution || distribution.kind === "unavailable") return;
    try {
      localStorage.removeItem(dismissalKey(distribution.kind));
      if (distribution.kind === "ccswitch")
        localStorage.removeItem(LEGACY_DISMISSED_KEY);
    } catch {
      // The in-memory reset still works.
    }
  }, [distribution]);

  // 应用启动时自动检查更新
  useEffect(() => {
    // 延迟1秒后检查，避免影响启动体验
    const timer = setTimeout(() => {
      runCheck(true).catch(console.error);
    }, 1000);

    return () => clearTimeout(timer);
  }, [runCheck]);

  const value: UpdateContextValue = {
    distribution,
    hasUpdate,
    updateInfo,
    isChecking,
    error,
    isDismissed,
    dismissUpdate,
    checkUpdate,
    resetDismiss,
  };

  return (
    <UpdateContext.Provider value={value}>{children}</UpdateContext.Provider>
  );
}

export function useUpdate() {
  const context = useContext(UpdateContext);
  if (!context) {
    throw new Error("useUpdate must be used within UpdateProvider");
  }
  return context;
}
