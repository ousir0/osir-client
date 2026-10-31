import React from "react";
import { invoke } from "@tauri-apps/api/core";
import { RefreshCw, TriangleAlert } from "lucide-react";
import i18n from "@/i18n";
import { reportFrontendError } from "@/lib/frontendLogger";
import "@/features/osir/tokens.css";
import "./frontend-error-boundary.css";

interface FrontendErrorBoundaryState {
  hasError: boolean;
}

export class FrontendErrorBoundary extends React.Component<
  React.PropsWithChildren,
  FrontendErrorBoundaryState
> {
  state: FrontendErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): FrontendErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo): void {
    if ("__TAURI_INTERNALS__" in window) {
      void invoke("osir_client_report_frontend_failure").catch(() => {});
    }
    reportFrontendError(
      "react.error_boundary",
      error,
      info.componentStack ?? undefined,
    );
  }

  render(): React.ReactNode {
    if (!this.state.hasError) {
      return this.props.children;
    }

    return (
      <main className="osir-frontend-error-boundary">
        <section role="alert" className="osir-frontend-error-panel">
          <div className="osir-frontend-error-copy">
            <TriangleAlert aria-hidden="true" />
            <div>
              <h1>
                {i18n.t("errors.frontendCrashTitle", {
                  defaultValue: "界面遇到了问题",
                })}
              </h1>
              <p>
                {i18n.t("errors.frontendCrashMessage", {
                  defaultValue:
                    "已尝试将错误信息写入应用诊断日志。请重新加载界面；如果问题持续，请在提交 Issue 时附上日志。",
                })}
              </p>
            </div>
          </div>
          <button
            type="button"
            className="osir-frontend-error-reload"
            onClick={() => window.location.reload()}
          >
            <RefreshCw aria-hidden="true" />
            {i18n.t("errors.reloadInterface", {
              defaultValue: "重新加载界面",
            })}
          </button>
        </section>
      </main>
    );
  }
}
