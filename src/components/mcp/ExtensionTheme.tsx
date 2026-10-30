import { createContext, useContext } from "react";
import { DropdownMenuContent } from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import type { ComponentProps } from "react";
import { HelpTip } from "@/components/ui/help-tip";
import { HoverTip } from "@/components/ui/hover-tip";

/** Portals need an explicit theme: they do not inherit the workbench's CSS. */
export const ExtensionThemeContext = createContext(false);

export function useExtensionOverlayClass() {
  return useContext(ExtensionThemeContext)
    ? "osir-drawer osir-extension-overlay"
    : undefined;
}

export function ExtensionMenuContent({
  className,
  ...props
}: ComponentProps<typeof DropdownMenuContent>) {
  const theme = useExtensionOverlayClass();
  return <DropdownMenuContent className={cn(theme, className)} {...props} />;
}

export function ExtensionHelpTip(props: ComponentProps<typeof HelpTip>) {
  const theme = useExtensionOverlayClass();
  return <HelpTip {...props} portalClassName={theme} />;
}

export function ExtensionHoverTip(props: ComponentProps<typeof HoverTip>) {
  const theme = useExtensionOverlayClass();
  return (
    <HoverTip
      {...props}
      portalClassName={theme ? `${theme} osir-extension-tooltip` : undefined}
    />
  );
}
