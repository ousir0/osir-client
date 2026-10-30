import { useEffect, useState, type ReactNode } from "react";
import "./account-pages.css";

// 只有明确 hidden 才暂停，兼容未实现 visibilityState 的 fixture。
const isVisible = () =>
  typeof document === "undefined" || document.visibilityState !== "hidden";

export interface OsirUsagePageProps {
  children: ReactNode;
}

export function OsirUsagePage({ children }: OsirUsagePageProps) {
  const [visible, setVisible] = useState(isVisible);
  useEffect(() => {
    const update = () => setVisible(isVisible());
    document.addEventListener("visibilitychange", update);
    update();
    return () => document.removeEventListener("visibilitychange", update);
  }, []);
  return (
    <section
      className="osir-account-pages osir-account-usage-page"
      aria-label="OSIR API 使用记录"
    >
      {visible ? children : null}
    </section>
  );
}
