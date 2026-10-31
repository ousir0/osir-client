export const CLAUDE_CONTEXT_TOKEN_DEFAULT = 200_000;
export const CLAUDE_CONTEXT_TOKEN_MAX = 2_000_000;
export const CLAUDE_AUTO_COMPACT_WINDOW_DEFAULT = 100_000;
export const CLAUDE_AUTO_COMPACT_WINDOW_MIN = 100_000;
export const CLAUDE_AUTO_COMPACT_WINDOW_MAX = 1_000_000;

const CONTEXT_ENV_KEY = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";
const AUTO_COMPACT_ENV_KEY = "CLAUDE_CODE_AUTO_COMPACT_WINDOW";

export function readClaudeContextLimit(configText: string): string {
  return readEnvLimit(configText, CONTEXT_ENV_KEY);
}

export function readClaudeAutoCompactWindow(configText: string): string {
  return readEnvLimit(configText, AUTO_COMPACT_ENV_KEY);
}

function readEnvLimit(configText: string, key: string): string {
  try {
    const parsed = JSON.parse(configText || "{}") as {
      env?: Record<string, unknown>;
    };
    const value = parsed.env?.[key];
    if (typeof value !== "string" && typeof value !== "number") return "";
    const text = String(value);
    return /^[1-9]\d*$/.test(text) && Number.isSafeInteger(Number(text))
      ? text
      : "";
  } catch {
    return "";
  }
}

export function updateClaudeContextLimit(
  configText: string,
  rawValue: string,
): string | null {
  return updateEnvLimit(configText, rawValue, CONTEXT_ENV_KEY, 1);
}

export function updateClaudeAutoCompactWindow(
  configText: string,
  rawValue: string,
): string | null {
  return updateEnvLimit(
    configText,
    rawValue,
    AUTO_COMPACT_ENV_KEY,
    CLAUDE_AUTO_COMPACT_WINDOW_MIN,
    CLAUDE_AUTO_COMPACT_WINDOW_MAX,
  );
}

function updateEnvLimit(
  configText: string,
  rawValue: string,
  key: string,
  minimum: number,
  maximum = CLAUDE_CONTEXT_TOKEN_MAX,
): string | null {
  let config: Record<string, unknown>;
  try {
    const parsed = JSON.parse(configText || "{}");
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
      return null;
    config = parsed as Record<string, unknown>;
  } catch {
    return null;
  }

  const value = rawValue.trim();
  if (value) {
    if (!/^\d+$/.test(value)) return null;
    const limit = Number(value);
    if (!Number.isSafeInteger(limit) || limit < minimum || limit > maximum) {
      return null;
    }
  }

  const env = isRecord(config.env) ? config.env : {};
  if (value) env[key] = value;
  else delete env[key];
  if (Object.keys(env).length > 0) config.env = env;
  else delete config.env;

  return JSON.stringify(config, null, 2);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
