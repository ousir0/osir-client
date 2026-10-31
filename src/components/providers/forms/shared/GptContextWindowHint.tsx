import { useTranslation } from "react-i18next";

import {
  GPT_CONTEXT_WINDOW_CAP,
  isGptModelId,
  positiveInteger,
} from "@/lib/modelMetadata";
import { cn } from "@/lib/utils";

/**
 * GPT 模型的窗口填得比订阅窗口大时，在输入框下提醒一句。手填的值照用不拦，
 * 只是中转站的 GPT 多半来自订阅，写大了要等上游拒收才失败。
 */
export function GptContextWindowHint({
  modelId,
  contextWindow,
  className,
}: {
  modelId: string | undefined;
  contextWindow: unknown;
  className?: string;
}) {
  const { t } = useTranslation();
  const window = positiveInteger(contextWindow);
  if (
    !modelId ||
    !isGptModelId(modelId) ||
    window === undefined ||
    window <= GPT_CONTEXT_WINDOW_CAP
  ) {
    return null;
  }
  return (
    <p className={cn("text-xs leading-relaxed text-warning-text", className)}>
      {t("providerForm.gptContextWindowHint", {
        defaultValue:
          "建议先使用 272K 或供应商支持的更小窗口。更大窗口可能增加费用，例如 GPT-5.5 的输入超过 272K 后会使用长上下文价格。请确认当前供应商和模型支持后再提高。",
      })}
    </p>
  );
}
