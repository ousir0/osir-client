import { getIdentifier } from "@tauri-apps/api/app";

/** Only upstream keeps eager startup work. OSIR views own resource reads. */
export async function resolveStartupPolicy(native: boolean) {
  if (!native) return { kind: "preview", backgroundStartup: false } as const;
  try {
    const identifier = await getIdentifier();
    if (identifier === "com.ccswitch.desktop") {
      return { kind: "ccswitch", backgroundStartup: true } as const;
    }
    if (
      identifier === "com.osir.client" ||
      identifier === "com.osir.client.dev" ||
      identifier === "com.osir.client.beta"
    ) {
      return { kind: "osir", backgroundStartup: false } as const;
    }
  } catch {
    // Required initialization diagnostics remain available if identity fails.
  }
  return { kind: "unknown", backgroundStartup: false } as const;
}
