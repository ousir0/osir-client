import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { FormLabel } from "@/components/ui/form";
import { Download, Info, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { HelpTip } from "@/components/ui/help-tip";
import { toast } from "@/lib/toast";
import EndpointSpeedTest from "./EndpointSpeedTest";
import { ApiKeySection, EndpointField, ModelInputWithFetch } from "./shared";
import {
  fetchModelsForConfig,
  getAgyModels,
  showFetchModelsError,
  type FetchedModel,
} from "@/lib/api/model-fetch";
import type { ProviderCategory } from "@/types";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  GEMINI_HISTORY_TOKEN_DEFAULT,
  GEMINI_HISTORY_TOKEN_MAX,
  readGeminiContextManagementEnabled,
  readGeminiHistoryTokenLimit,
  updateGeminiContextManagementEnabled,
  updateGeminiHistoryTokenLimit,
} from "./geminiContextLimit";

interface EndpointCandidate {
  url: string;
}

interface GeminiFormFieldsProps {
  appId?: "gemini" | "agy";
  providerId?: string;
  // API Key
  shouldShowApiKey: boolean;
  apiKey: string;
  onApiKeyChange: (key: string) => void;
  category?: ProviderCategory;
  shouldShowApiKeyLink: boolean;
  websiteUrl: string;
  isPartner?: boolean;
  partnerPromotionKey?: string;

  // Base URL
  shouldShowSpeedTest: boolean;
  baseUrl: string;
  onBaseUrlChange: (url: string) => void;
  isEndpointModalOpen: boolean;
  onEndpointModalToggle: (open: boolean) => void;
  onCustomEndpointsChange: (endpoints: string[]) => void;
  autoSelect: boolean;
  onAutoSelectChange: (checked: boolean) => void;

  // Model
  shouldShowModelField: boolean;
  model: string;
  onModelChange: (value: string) => void;
  configValue: string;
  onConfigChange: (value: string) => void;

  // Speed Test Endpoints
  speedTestEndpoints: EndpointCandidate[];
}

export function GeminiFormFields({
  appId = "gemini",
  providerId,
  shouldShowApiKey,
  apiKey,
  onApiKeyChange,
  category,
  shouldShowApiKeyLink,
  websiteUrl,
  isPartner,
  partnerPromotionKey,
  shouldShowSpeedTest,
  baseUrl,
  onBaseUrlChange,
  isEndpointModalOpen,
  onEndpointModalToggle,
  onCustomEndpointsChange,
  autoSelect,
  onAutoSelectChange,
  shouldShowModelField,
  model,
  onModelChange,
  configValue,
  onConfigChange,
  speedTestEndpoints,
}: GeminiFormFieldsProps) {
  const { t } = useTranslation();

  const [fetchedModels, setFetchedModels] = useState<FetchedModel[]>([]);
  const [isFetchingModels, setIsFetchingModels] = useState(false);

  const handleFetchModels = useCallback(() => {
    // agy 两种模式都只认它自己清单里的显示名，接口的模型 ID 对不上，所以一律问本机 agy。
    const agy = appId === "agy";
    const modelBaseUrl = baseUrl.trim();
    if (!agy && (!modelBaseUrl || !apiKey)) {
      showFetchModelsError(null, t, {
        hasApiKey: !!apiKey,
        hasBaseUrl: !!modelBaseUrl,
      });
      return;
    }
    setIsFetchingModels(true);
    (agy
      ? getAgyModels({ apiKey, baseUrl: modelBaseUrl })
      : fetchModelsForConfig(modelBaseUrl, apiKey, false)
    )
      .then((models) => {
        setFetchedModels(models);
        if (models.length === 0) {
          toast.info(t("providerForm.fetchModelsEmpty"));
        } else {
          toast.success(
            t("providerForm.fetchModelsSuccess", { count: models.length }),
          );
        }
      })
      .catch((err) => {
        console.warn("[ModelFetch] Failed:", err);
        showFetchModelsError(err, t);
      })
      .finally(() => setIsFetchingModels(false));
  }, [appId, baseUrl, apiKey, t]);

  // 检测是否为 Google 官方（使用 OAuth）
  const isGoogleOfficial =
    partnerPromotionKey?.toLowerCase() === "google-official";

  return (
    <>
      {/* Google OAuth 提示 */}
      {isGoogleOfficial && (
        <div className="rounded-lg border border-border-strong bg-subtle p-4">
          <div className="flex gap-3">
            <Info className="h-5 w-5 flex-shrink-0 text-fg-1" />
            <div className="space-y-1">
              <p className="text-sm font-medium text-fg-1">
                {t("provider.form.gemini.oauthTitle", {
                  defaultValue: "OAuth 认证模式",
                })}
              </p>
              <p className="text-sm text-fg-1">
                {t("provider.form.gemini.oauthHint", {
                  defaultValue:
                    "Google 官方使用 OAuth 个人认证，无需填写 API Key。首次使用时会自动打开浏览器进行登录。",
                })}
              </p>
            </div>
          </div>
        </div>
      )}

      {/* API Key 输入框 */}
      {shouldShowApiKey && !isGoogleOfficial && (
        <ApiKeySection
          value={apiKey}
          onChange={onApiKeyChange}
          category={category}
          required
          shouldShowLink={shouldShowApiKeyLink}
          websiteUrl={websiteUrl}
          isPartner={isPartner}
          partnerPromotionKey={partnerPromotionKey}
        />
      )}

      {/* Base URL 输入框（统一使用与 Codex 相同的样式与交互） */}
      {shouldShowSpeedTest && (
        <EndpointField
          id="baseUrl"
          required={appId !== "agy"}
          label={t("providerForm.apiEndpoint", { defaultValue: "API 端点" })}
          value={baseUrl}
          onChange={onBaseUrlChange}
          placeholder={t("providerForm.apiEndpointPlaceholder", {
            defaultValue: "https://your-api-endpoint.com/",
          })}
          help={
            appId === "agy" ? t("provider.form.agy.endpointHelp") : undefined
          }
          onManageClick={() => onEndpointModalToggle(true)}
        />
      )}

      {/* Model 输入框 */}
      {shouldShowModelField && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-0.5">
              <FormLabel htmlFor="gemini-model">
                {t("provider.form.gemini.model", { defaultValue: "模型" })}
              </FormLabel>
              {appId === "agy" && (
                <HelpTip
                  title={t("provider.form.gemini.model", {
                    defaultValue: "模型",
                  })}
                >
                  {t("provider.form.agy.modelHelp")}
                </HelpTip>
              )}
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handleFetchModels}
              disabled={isFetchingModels}
              className="h-7 gap-1"
            >
              {isFetchingModels ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Download className="h-3.5 w-3.5" />
              )}
              {t("providerForm.fetchModels")}
            </Button>
          </div>
          <ModelInputWithFetch
            id="gemini-model"
            value={model}
            onChange={onModelChange}
            placeholder={
              appId === "agy" && !apiKey
                ? "Gemini 3.8 Flash (High)"
                : "gemini-3.6-flash"
            }
            fetchedModels={fetchedModels}
            isLoading={isFetchingModels}
          />
        </div>
      )}

      {appId === "gemini" && (
        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-3">
            <FormLabel htmlFor="gemini-context-management-enabled">
              {t("geminiConfig.contextManagementEnabled", {
                defaultValue: "Automatic history compaction",
              })}
            </FormLabel>
            <Switch
              id="gemini-context-management-enabled"
              checked={readGeminiContextManagementEnabled(configValue)}
              onCheckedChange={(enabled) => {
                const next = updateGeminiContextManagementEnabled(
                  configValue,
                  enabled,
                );
                if (next !== null) onConfigChange(next);
              }}
              aria-label={t("geminiConfig.contextManagementEnabled", {
                defaultValue: "Automatic history compaction",
              })}
            />
          </div>
          <p className="text-xs text-fg-2">
            {t("geminiConfig.contextManagementEnabledHint", {
              defaultValue:
                "Compress older conversation history when it grows beyond the configured window.",
            })}
          </p>
          <FormLabel htmlFor="gemini-history-token-limit">
            {t("geminiConfig.historyWindowMaxTokens", {
              defaultValue: "History window limit",
            })}
          </FormLabel>
          <Input
            id="gemini-history-token-limit"
            type="number"
            min={1}
            max={GEMINI_HISTORY_TOKEN_MAX}
            step={1}
            inputMode="numeric"
            disabled={!readGeminiContextManagementEnabled(configValue)}
            value={readGeminiHistoryTokenLimit(configValue)}
            placeholder={String(GEMINI_HISTORY_TOKEN_DEFAULT)}
            onChange={(event) => {
              const next = updateGeminiHistoryTokenLimit(
                configValue,
                event.target.value,
              );
              if (next !== null) onConfigChange(next);
            }}
            aria-describedby="gemini-history-token-limit-help"
          />
          <p id="gemini-history-token-limit-help" className="text-xs text-fg-2">
            {t("geminiConfig.historyWindowMaxTokensHint", {
              defaultValue:
                "Compression starts after this many tokens while enabled. Leave blank for the recommended 150,000. Restart Gemini CLI to apply.",
            })}
          </p>
        </div>
      )}

      {/* 端点测速弹窗 */}
      {shouldShowSpeedTest && isEndpointModalOpen && (
        <EndpointSpeedTest
          appId={appId}
          providerId={providerId}
          value={baseUrl}
          onChange={onBaseUrlChange}
          initialEndpoints={speedTestEndpoints}
          visible={isEndpointModalOpen}
          onClose={() => onEndpointModalToggle(false)}
          autoSelect={autoSelect}
          onAutoSelectChange={onAutoSelectChange}
          onCustomEndpointsChange={onCustomEndpointsChange}
        />
      )}
    </>
  );
}
