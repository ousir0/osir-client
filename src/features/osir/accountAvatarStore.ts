import { useCallback, useSyncExternalStore } from "react";

const EVENT = "osir-client-avatar-change";
const MAX_STORED_CHARS = 512_000;
export const MAX_AVATAR_BYTES = 5 * 1024 * 1024;

function keyFor(accountId: number | null | undefined) {
  return accountId != null && Number.isSafeInteger(accountId) && accountId > 0
    ? `osir_client.avatar.v1.${accountId}`
    : null;
}

function safeData(value: string | null) {
  return value != null &&
    value.length <= MAX_STORED_CHARS &&
    /^data:image\/png;base64,[A-Za-z0-9+/]+={0,2}$/.test(value)
    ? value
    : null;
}

export function readAccountAvatar(accountId: number | null | undefined) {
  const key = keyFor(accountId);
  if (!key) return null;
  try {
    return safeData(localStorage.getItem(key));
  } catch {
    return null;
  }
}

/** Local appearance preference only; never part of account APIs or telemetry. */
export function saveAccountAvatar(accountId: number, image: string | null) {
  const key = keyFor(accountId);
  if (!key || (image !== null && safeData(image) === null))
    throw new Error("图片无法保存，请重新选择。");
  try {
    if (image === null) localStorage.removeItem(key);
    else localStorage.setItem(key, image);
  } catch {
    throw new Error("头像未保存，请检查本机存储空间后重试。");
  }
  window.dispatchEvent(new CustomEvent(EVENT, { detail: key }));
}

export function useAccountAvatar(accountId: number | null | undefined) {
  const key = keyFor(accountId);
  const subscribe = useCallback(
    (notify: () => void) => {
      const changed = (event: Event) => {
        if (
          (event instanceof StorageEvent &&
            (event.key === key || event.key === null)) ||
          (event instanceof CustomEvent && event.detail === key)
        )
          notify();
      };
      window.addEventListener(EVENT, changed);
      window.addEventListener("storage", changed);
      return () => {
        window.removeEventListener(EVENT, changed);
        window.removeEventListener("storage", changed);
      };
    },
    [key],
  );
  return useSyncExternalStore(
    subscribe,
    () => readAccountAvatar(accountId),
    () => null,
  );
}

/** Check bytes as well as MIME: SVG, HTML and renamed files never reach img. */
export async function validateAvatarFile(file: File) {
  if (file.size === 0 || file.size > MAX_AVATAR_BYTES)
    throw new Error("请选择不超过 5 MB 的图片。");
  const bytes = new Uint8Array(await file.slice(0, 12).arrayBuffer());
  const png = [137, 80, 78, 71, 13, 10, 26, 10].every((v, i) => bytes[i] === v);
  const jpeg = bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255;
  const webp =
    String.fromCharCode(...bytes.slice(0, 4)) === "RIFF" &&
    String.fromCharCode(...bytes.slice(8, 12)) === "WEBP";
  if (
    !(
      (file.type === "image/png" && png) ||
      (file.type === "image/jpeg" && jpeg) ||
      (file.type === "image/webp" && webp)
    )
  )
    throw new Error("请选择 PNG、JPG 或 WebP 图片。");
}

/** Re-encode a square crop, removing EXIF/metadata and limiting stored size. */
export async function prepareAccountAvatar(file: File): Promise<string> {
  await validateAvatarFile(file);
  const url = URL.createObjectURL(file);
  try {
    const image = await new Promise<HTMLImageElement>((resolve, reject) => {
      const img = new Image();
      const timer = window.setTimeout(
        () => reject(new Error("图片读取超时，请重新选择。")),
        10_000,
      );
      img.onload = () => {
        window.clearTimeout(timer);
        resolve(img);
      };
      img.onerror = () => {
        window.clearTimeout(timer);
        reject(new Error("无法读取这张图片，请选择其他图片。"));
      };
      img.src = url;
    });
    if (
      !image.naturalWidth ||
      !image.naturalHeight ||
      image.naturalWidth > 4096 ||
      image.naturalHeight > 4096
    )
      throw new Error("图片尺寸过大，请选择 4096 × 4096 像素以内的图片。");
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 256;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("无法处理图片，请重新选择。");
    const side = Math.min(image.naturalWidth, image.naturalHeight);
    context.drawImage(
      image,
      (image.naturalWidth - side) / 2,
      (image.naturalHeight - side) / 2,
      side,
      side,
      0,
      0,
      256,
      256,
    );
    const data = canvas.toDataURL("image/png");
    if (safeData(data) === null)
      throw new Error("图片过于复杂，请选择其他图片。");
    return data;
  } finally {
    URL.revokeObjectURL(url);
  }
}
