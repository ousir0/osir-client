import { Toaster } from "sonner";
import "./toast.css";

/** One OSIR feedback surface for both native and isolated browser previews. */
export function OsirToaster() {
  return (
    <Toaster
      theme="light"
      position="bottom-center"
      visibleToasts={3}
      closeButton
      toastOptions={{
        duration: 2000,
        classNames: {
          toast: "osir-toast",
          title: "osir-toast-title",
          description: "osir-toast-description",
          closeButton: "osir-toast-close",
        },
      }}
    />
  );
}
