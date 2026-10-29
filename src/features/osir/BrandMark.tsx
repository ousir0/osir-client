import { useState } from "react";
import { Cloud, Server } from "lucide-react";
import { ClaudeIcon, CodexIcon, GeminiIcon } from "@/components/BrandIcons";
import { ProviderIcon } from "@/components/ProviderIcon";
import { hasIcon, isUrlIcon } from "@/icons/extracted";
import { isCustomIcon } from "@/utils/customIcon";
import type { Provider } from "@/types";
import { modelNames, readConnection, type ToolId } from "./providerConfig";

type ToolProps = { tool: ToolId; size?: number };
type ConnectionProps = ToolProps & { provider: Provider };

/** Supports JSX props and the small render-helper form ToolBrandIcon(tool, size). */
export function ToolBrandIcon(props: ToolProps | ToolId, iconSize = 20) {
  const { tool, size = 20 } =
    typeof props === "string" ? { tool: props, size: iconSize } : props;
  switch (tool) {
    case "codex":
      return <CodexIcon size={size} />;
    case "claude":
      return <ClaudeIcon size={size} />;
    case "gemini":
      return <GeminiIcon size={size} />;
    case "opencode":
      return <BrandGlyph icon="opencode" name="OpenCode" size={size} />;
  }
}

const aliases: Record<string, string> = {
  openai: "openai",
  anthropic: "anthropic",
  claude: "anthropic",
  google: "google",
  gemini: "google",
  deepseek: "deepseek",
  qwen: "qwen",
  alibaba: "alibaba",
  moonshot: "kimi",
  kimi: "kimi",
  mistral: "mistral",
  xai: "xai",
  grok: "xai",
  minimax: "minimax",
  zhipu: "zhipu",
  glm: "zhipu",
  openrouter: "openrouter",
  siliconflow: "siliconflow",
  ollama: "ollama",
};
function namedBrand(value: unknown) {
  return typeof value === "string"
    ? aliases[value.toLowerCase().trim()]
    : undefined;
}
function endpointBrand(endpoint: string) {
  try {
    const host = new URL(endpoint).hostname.toLowerCase();
    const domains: Record<string, string> = {
      "openai.com": "openai",
      "anthropic.com": "anthropic",
      "generativelanguage.googleapis.com": "google",
      "aiplatform.googleapis.com": "google",
      "deepseek.com": "deepseek",
      "openrouter.ai": "openrouter",
      "siliconflow.cn": "siliconflow",
      "siliconflow.com": "siliconflow",
      "moonshot.cn": "kimi",
      "moonshot.ai": "kimi",
      "mistral.ai": "mistral",
      "x.ai": "xai",
      "minimax.io": "minimax",
      "minimaxi.com": "minimax",
      "bigmodel.cn": "zhipu",
      "dashscope.aliyuncs.com": "qwen",
    };
    return Object.entries(domains).find(
      ([domain]) => host === domain || host.endsWith("." + domain),
    )?.[1];
  } catch {
    return undefined;
  }
}
function modelBrand(model: string) {
  // A protocol name or the tool itself is never evidence of model ownership.
  const name = model.toLowerCase().split("/").at(-1) || "";
  if (
    /^(gpt[-\d]|chatgpt[-\d]|o[134](?:-|$)|codex(?:-|$)|dall-e|gpt-image)/.test(
      name,
    )
  )
    return "openai";
  if (/^claude(?:-|$)/.test(name)) return "anthropic";
  if (/^(gemini|gemma)(?:-|$)/.test(name)) return "google";
  if (/^deepseek(?:-|$)/.test(name)) return "deepseek";
  if (/^qwen(?:[-\d]|$)/.test(name)) return "qwen";
  if (/^(kimi|moonshot)(?:-|$)/.test(name)) return "kimi";
  if (/^(mistral|mixtral|codestral|magistral)(?:-|$)/.test(name))
    return "mistral";
  if (/^grok(?:-|$)/.test(name)) return "xai";
  if (/^minimax(?:-|$)/.test(name)) return "minimax";
  if (/^glm(?:-|$)/.test(name)) return "zhipu";
  return undefined;
}
function inferredIcon(provider: Provider, tool: ToolId) {
  // Read only explicit brand metadata. apiFormat describes transport compatibility.
  const binding = provider.meta?.osirBinding?.binding;
  const metadata =
    namedBrand(provider.meta?.providerType) || namedBrand(binding?.platform);
  if (metadata) return metadata;
  let endpoint = "";
  try {
    endpoint = readConnection(provider, tool).baseUrl;
  } catch {
    /* Malformed legacy configuration stays neutral. */
  }
  const hostBrand = endpointBrand(endpoint);
  if (hostBrand) return hostBrand;
  const models = modelNames(provider, tool);
  const brands = models.map(modelBrand);
  // Mixed catalogs and unknown models cannot identify a single supplier brand.
  if (
    brands.length &&
    brands[0] &&
    brands.every((brand) => brand === brands[0])
  )
    return brands[0];
  return undefined;
}

export function connectionBrandName(provider: Provider, tool: ToolId) {
  const brand = inferredIcon(provider, tool);
  const names: Record<string, string> = {
    openai: "OpenAI",
    anthropic: "Anthropic",
    google: "Google",
    deepseek: "DeepSeek",
    qwen: "Qwen",
    alibaba: "Alibaba",
    kimi: "Kimi",
    mistral: "Mistral",
    xai: "xAI",
    minimax: "MiniMax",
    zhipu: "智谱",
    openrouter: "OpenRouter",
    siliconflow: "SiliconFlow",
    ollama: "Ollama",
  };
  return brand ? names[brand] || provider.name : provider.name;
}

function BrandGlyph({
  icon,
  name,
  size,
  color,
  local = false,
}: {
  icon?: string;
  name: string;
  size: number;
  color?: string;
  local?: boolean;
}) {
  const [broken, setBroken] = useState<string>();
  const valid =
    icon && (hasIcon(icon) || isUrlIcon(icon) || isCustomIcon(icon));
  if (!valid || broken === icon) {
    const Fallback = local ? Server : Cloud;
    return <Fallback size={size} aria-label={name} role="img" />;
  }
  return (
    <span
      className="inline-flex shrink-0"
      style={{ width: size, height: size }}
      onErrorCapture={() => setBroken(icon)}
    >
      <ProviderIcon
        icon={icon}
        name={name}
        size={size}
        color={color}
        showFallback={false}
      />
    </span>
  );
}

/** Explicit provider icons win; OSIR bindings alone never imply OpenAI. */
export function ConnectionBrandIcon(
  props: ConnectionProps | Provider,
  tool?: ToolId,
  iconSize = 24,
) {
  const options: ConnectionProps =
    "provider" in props
      ? props
      : { provider: props, tool: tool || "codex", size: iconSize };
  const { provider, size = 24 } = options;
  return (
    <BrandGlyph
      icon={provider.icon?.trim() || inferredIcon(provider, options.tool)}
      name={provider.name}
      size={size}
      color={provider.iconColor}
      local={provider.category === "local"}
    />
  );
}
