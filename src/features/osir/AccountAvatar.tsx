import { useEffect, useRef, useState } from "react";
import { Camera, ImagePlus, RotateCcw } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import defaultAvatar from "@/assets/brand/osir-avatar.png";
import {
  prepareAccountAvatar,
  saveAccountAvatar,
  useAccountAvatar,
} from "./accountAvatarStore";
import "./account-avatar.css";

export function AvatarImage({
  src,
  size,
}: {
  src: string | null;
  size: number;
}) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [src]);
  return (
    <img
      src={failed || !src ? defaultAvatar : src}
      alt=""
      width={size}
      height={size}
      className={
        src && !failed ? "osir-avatar-image is-custom" : "osir-avatar-image"
      }
      onError={() => setFailed(true)}
      draggable={false}
    />
  );
}

export function AccountAvatar({
  accountId,
  size = 56,
  label = "更换头像",
}: {
  accountId: number | null | undefined;
  size?: number;
  label?: string;
}) {
  const current = useAccountAvatar(accountId);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const epoch = useRef(0);
  useEffect(() => {
    epoch.current += 1;
    setOpen(false);
    setBusy(false);
  }, [accountId]);
  function close() {
    epoch.current += 1;
    setOpen(false);
    setBusy(false);
  }
  async function choose(file: File | undefined) {
    if (!file) return;
    const request = ++epoch.current;
    setBusy(true);
    setError(null);
    try {
      const image = await prepareAccountAvatar(file);
      if (epoch.current === request) setDraft(image);
    } catch (failure) {
      if (epoch.current === request)
        setError(
          failure instanceof Error
            ? failure.message
            : "无法读取图片，请重新选择。",
        );
    } finally {
      if (epoch.current === request) setBusy(false);
    }
  }
  const canChange =
    accountId != null && Number.isSafeInteger(accountId) && accountId > 0;
  return (
    <>
      <button
        type="button"
        className="osir-avatar-button"
        aria-label={label}
        title={canChange ? "更换头像" : "登录后更换头像"}
        disabled={!canChange}
        style={{ width: size, height: size }}
        onClick={() => {
          setDraft(current);
          setError(null);
          setOpen(true);
        }}
      >
        <AvatarImage src={current} size={size} />
        <Camera size={13} className="osir-avatar-camera" aria-hidden="true" />
      </button>
      <Dialog open={open} onOpenChange={(next) => !next && close()}>
        <DialogContent
          className="osir-avatar-dialog"
          zIndex="top"
          aria-describedby={undefined}
        >
          <DialogHeader>
            <DialogTitle>更换头像</DialogTitle>
          </DialogHeader>
          <div className="osir-avatar-editor">
            <AvatarImage src={draft} size={160} />
            <input
              ref={input}
              type="file"
              accept="image/png,image/jpeg,image/webp"
              aria-label="选择头像图片"
              hidden
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = "";
                void choose(file);
              }}
            />
            <div className="osir-avatar-options">
              <button
                type="button"
                disabled={busy}
                onClick={() => input.current?.click()}
              >
                <ImagePlus size={15} />
                选择图片
              </button>
              <button
                type="button"
                disabled={busy || draft === null}
                onClick={() => {
                  setDraft(null);
                  setError(null);
                }}
              >
                <RotateCcw size={15} />
                恢复默认
              </button>
            </div>
            <p className="osir-avatar-hint">
              PNG、JPG 或 WebP，最大 5 MB；居中裁剪为正方形。
            </p>
            {busy && <p role="status">正在处理图片…</p>}
            {error && <p role="alert">{error}</p>}
          </div>
          <DialogFooter>
            <button type="button" onClick={close}>
              取消
            </button>
            <button
              type="button"
              className="osir-avatar-save"
              disabled={busy || draft === current || !canChange}
              onClick={() => {
                if (!canChange || accountId == null) return;
                try {
                  saveAccountAvatar(accountId, draft);
                  close();
                } catch (failure) {
                  setError(
                    failure instanceof Error
                      ? failure.message
                      : "头像未保存，请重试。",
                  );
                }
              }}
            >
              保存
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
