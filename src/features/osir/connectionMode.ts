export type NativeConfiguration = {
  source: "official_login" | "local_config" | "unknown";
  status: "available" | "unavailable" | "unknown";
  label: string;
  detail?: string;
};

export type ConnectionModeView = {
  mode: "native" | "api_key" | "osir_api" | "unavailable";
  canRestoreNative: boolean;
  reason?: string;
  nativeConfiguration?: NativeConfiguration;
};

// Accept the previous native DTO during mixed-version previews and upgrades.
export function connectionKind(mode: ConnectionModeView["mode"] | undefined) {
  return mode === "api_key" || mode === "osir_api"
    ? "api_key"
    : mode === "native"
      ? "native"
      : undefined;
}
