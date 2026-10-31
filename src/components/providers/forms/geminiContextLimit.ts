export const GEMINI_HISTORY_TOKEN_DEFAULT = 150_000;
export const GEMINI_HISTORY_TOKEN_MAX = 2_000_000;

export function readGeminiContextManagementEnabled(
  configText: string,
): boolean {
  try {
    const config = JSON.parse(configText || "{}") as {
      experimental?: { contextManagement?: unknown };
    };
    const enabled = config.experimental?.contextManagement;
    return typeof enabled === "boolean" ? enabled : true;
  } catch {
    return true;
  }
}

export function readGeminiHistoryTokenLimit(configText: string): string {
  try {
    const config = JSON.parse(configText) as {
      contextManagement?: { historyWindow?: { maxTokens?: unknown } };
    };
    const limit = config.contextManagement?.historyWindow?.maxTokens;
    return typeof limit === "number" && Number.isSafeInteger(limit) && limit > 0
      ? String(limit)
      : "";
  } catch {
    return "";
  }
}

export function updateGeminiContextManagementEnabled(
  configText: string,
  enabled: boolean,
): string | null {
  const config = readConfig(configText);
  if (!config) return null;

  const experimental = isRecord(config.experimental) ? config.experimental : {};
  experimental.contextManagement = enabled;
  config.experimental = experimental;
  return JSON.stringify(config, null, 2);
}

export function updateGeminiHistoryTokenLimit(
  configText: string,
  rawValue: string,
): string | null {
  const config = readConfig(configText);
  if (!config) return null;

  const value = rawValue.trim();
  let limit: number | undefined;
  if (value) {
    if (!/^\d+$/.test(value)) return null;
    limit = Number(value);
    if (
      !Number.isSafeInteger(limit) ||
      limit < 1 ||
      limit > GEMINI_HISTORY_TOKEN_MAX
    ) {
      return null;
    }
  }

  const contextManagement = isRecord(config.contextManagement)
    ? config.contextManagement
    : {};
  const historyWindow = isRecord(contextManagement.historyWindow)
    ? contextManagement.historyWindow
    : {};
  if (limit === undefined) delete historyWindow.maxTokens;
  else historyWindow.maxTokens = limit;
  if (Object.keys(historyWindow).length > 0)
    contextManagement.historyWindow = historyWindow;
  else delete contextManagement.historyWindow;
  if (Object.keys(contextManagement).length > 0)
    config.contextManagement = contextManagement;
  else delete config.contextManagement;

  return JSON.stringify(config, null, 2);
}

function readConfig(configText: string): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(configText || "{}");
    return typeof parsed === "object" &&
      parsed !== null &&
      !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
