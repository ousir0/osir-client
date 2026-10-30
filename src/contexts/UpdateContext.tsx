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
const UPDATE_CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;
const MIN_FOCUS_CHECK_INTERVAL_MS = 5 * 60 * 1000;
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

  const checkPromiseRef = useRef<Promise<boolean> | null>(null);
  const lastAutomaticCheckAtRef = useRef<number | null>(null);

  const runCheck = useCallback(async (automatic: boolean) => {
    if (checkPromiseRef.current) return checkPromiseRef.current;
    setIsChecking(true);
    setError(null);

    const pending = (async () => {
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
          setIsDismissed(
            readDismissedVersion(capability.kind) ===
              result.info.availableVersion,
          );
          return true;
        }

        setHasUpdate(false);
        setUpdateInfo(null);
        setIsDismissed(false);
        return false;
      } catch (err) {
        setError(extractErrorMessage(err) || "检查更新失败");
        setHasUpdate(false);
        throw err;
      } finally {
        setIsChecking(false);
      }
    })();
    checkPromiseRef.current = pending;
    try {
      return await pending;
    } finally {
      if (checkPromiseRef.current === pending) checkPromiseRef.current = null;
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

  // Recheck while the app stays open so a release made after launch is found.
  useEffect(() => {
    const automaticCheck = () => {
      if (document.visibilityState === "hidden") return;
      const now = Date.now();
      const last = lastAutomaticCheckAtRef.current;
      if (last !== null && now - last < MIN_FOCUS_CHECK_INTERVAL_MS) return;
      lastAutomaticCheckAtRef.current = now;
      void runCheck(true).catch(console.error);
    };
    const startupTimer = setTimeout(automaticCheck, 1000);
    const interval = setInterval(automaticCheck, UPDATE_CHECK_INTERVAL_MS);
    window.addEventListener("focus", automaticCheck);
    document.addEventListener("visibilitychange", automaticCheck);

    return () => {
      clearTimeout(startupTimer);
      clearInterval(interval);
      window.removeEventListener("focus", automaticCheck);
      document.removeEventListener("visibilitychange", automaticCheck);
    };
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
