import { getVersion } from "@tauri-apps/api/app";

export type UpdateChannel = "stable" | "beta";

export interface UpdateInfo {
  currentVersion: string;
  availableVersion: string;
  // Full native version identifies a release independently of its display text.
  availableVersionId?: string;
  notes?: string;
  pubDate?: string;
}

export interface CheckOptions {
  timeout?: number;
  channel?: UpdateChannel;
}

export function displayClientVersion(version: string): string {
  return version.split("+", 1)[0];
}

export async function getCurrentVersion(): Promise<string> {
  try {
    return displayClientVersion(await getVersion());
  } catch {
    return "";
  }
}

export async function checkForUpdate(
  opts: CheckOptions = {},
): Promise<
  { status: "up-to-date" } | { status: "available"; info: UpdateInfo }
> {
  // 动态引入，避免在未安装插件时导致打包期问题
  const { check } = await import("@tauri-apps/plugin-updater");

  const currentVersion = await getCurrentVersion();
  const update = await check({ timeout: opts.timeout ?? 30000 });

  if (!update) {
    return { status: "up-to-date" };
  }

  const availableVersion = displayClientVersion(update.version ?? "");
  const notes = update.body ?? (update as { notes?: string }).notes;
  const info: UpdateInfo = {
    currentVersion,
    availableVersion,
    ...(update.version?.includes("+")
      ? { availableVersionId: update.version }
      : {}),
    notes: update.version?.includes("+")
      ? [`${availableVersion} 修订更新`, notes].filter(Boolean).join("\n")
      : notes,
    pubDate: update.date,
  };

  return { status: "available", info };
}
