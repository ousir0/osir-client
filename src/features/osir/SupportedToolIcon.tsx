import { useState } from "react";
import { Terminal } from "lucide-react";
import { OpenClawIcon } from "@/components/BrandIcons";
import { ProviderIcon } from "@/components/ProviderIcon";
import workbuddyIcon from "@/assets/tools/workbuddy.png";
import { ToolBrandIcon } from "./BrandMark";
import {
  LOCAL_TOOL_CATALOG,
  isProviderTool,
  type SupportedToolId,
} from "./toolsApi";

const providerIcons = {
  grok: "grok",
  hermes: "hermes",
  pi: "pi",
  mcode: "minimax",
  kimi: "kimi",
  agy: "antigravity",
  omp: "omp",
} satisfies Record<
  Exclude<
    SupportedToolId,
    | "codex"
    | "claude"
    | "claude-desktop"
    | "opencode"
    | "gemini"
    | "openclaw"
    | "workbuddy"
  >,
  string
>;

/** Local brand assets shared by navigation, tool cards and version management. */
export function SupportedToolIcon({
  id,
  size = 20,
  className,
  decorative = false,
}: {
  id: SupportedToolId;
  size?: number;
  className?: string;
  decorative?: boolean;
}) {
  const [brokenId, setBrokenId] = useState<string>();
  const name = LOCAL_TOOL_CATALOG[id]?.name ?? "本机工具";
  let icon;
  if (brokenId === id) {
    icon = <Terminal size={size} />;
  } else if (id === "claude-desktop") {
    icon = <ToolBrandIcon tool="claude" size={size} />;
  } else if (isProviderTool(id)) {
    icon = <ToolBrandIcon tool={id} size={size} />;
  } else if (id === "openclaw") {
    icon = <OpenClawIcon size={size} />;
  } else if (id === "workbuddy") {
    icon = <img src={workbuddyIcon} alt="" width={size} height={size} />;
  } else {
    const source = providerIcons[id];
    icon = source ? (
      <ProviderIcon
        icon={source}
        name={name}
        size={size}
        showFallback={false}
      />
    ) : (
      <Terminal size={size} />
    );
  }
  return (
    <span
      role={decorative ? undefined : "img"}
      aria-label={decorative ? undefined : name}
      aria-hidden={decorative || undefined}
      data-tool-icon={id}
      className={className}
      style={{
        display: "inline-flex",
        width: size,
        height: size,
        flexShrink: 0,
      }}
      onErrorCapture={() => setBrokenId(id)}
    >
      <span
        aria-hidden="true"
        style={{ display: "inline-flex", width: size, height: size }}
      >
        {icon}
      </span>
    </span>
  );
}
