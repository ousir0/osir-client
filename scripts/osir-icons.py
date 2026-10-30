#!/usr/bin/env python3
"""Generate every OSIR platform/channel/frontend icon from one transparent logo."""

import argparse
import hashlib
import io
import json
import math
import shutil
import struct
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, __version__ as pillow_version

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "assets/brand/osir-client"
OUTPUT = ROOT / "src-tauri/icons/osir"
UI_ICON = ROOT / "src/assets/brand/osir-client.png"
DEFAULT_AVATAR = ROOT / "src/assets/brand/osir-avatar.png"
RESAMPLE = Image.Resampling.LANCZOS
PNG_SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512, 1024)
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)
WINDOWS_TILES = (30, 44, 71, 89, 107, 142, 150, 284, 310)
CHANNELS = ("prod", "dev", "beta")
MAC_PLATE = (100, 100, 924, 924)
MAC_ARTWORK_SIZE = 704


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def master(path):
    with Image.open(path) as opened:
        if opened.format != "PNG" or opened.width != opened.height:
            raise ValueError("Logo master must be a square PNG")
        if not 192 <= opened.width <= 4096:
            raise ValueError("Logo master must be between 192 and 4096 pixels")
        image = opened.convert("RGBA")
    if any(image.getpixel(point)[3] for point in ((0, 0), (image.width - 1, 0),
                                                  (0, image.height - 1), (image.width - 1, image.height - 1))):
        raise ValueError("Logo master must have transparent outer corners")
    if image.getbbox() is None:
        raise ValueError("Logo master must contain visible artwork")
    return image.resize((1024, 1024), RESAMPLE)


def fitted(image, bounds, canvas_size=(1024, 1024)):
    artwork = image.crop(image.getbbox())
    artwork.thumbnail(bounds, RESAMPLE)
    canvas = Image.new("RGBA", canvas_size)
    canvas.alpha_composite(artwork, ((canvas_size[0] - artwork.width) // 2,
                                    (canvas_size[1] - artwork.height) // 2))
    return canvas


def channel_label(image, channel, y):
    if channel == "prod":
        return image
    draw = ImageDraw.Draw(image)
    font = ImageFont.load_default(size=62)
    draw.text((512, y), "DEV" if channel == "dev" else "BETA", font=font,
              fill="#355d83" if channel == "dev" else "#147e9c", anchor="mm")
    return image


def macos_icon(logo, channel):
    # 1024px Apple-style canvas: 824px rounded plate with100px outer safe area.
    # A superellipse gives continuous rounded corners instead of a circular tile.
    mask = Image.new("L", (2048, 2048))
    points = []
    for step in range(1440):
        angle = math.tau * step / 1440
        co, si = math.cos(angle), math.sin(angle)
        points.append((1024 + 824 * math.copysign(abs(co) ** (2 / 4.4), co),
                       1024 + 824 * math.copysign(abs(si) ** (2 / 4.4), si)))
    ImageDraw.Draw(mask).polygon(points, fill=255)
    mask = mask.resize((1024, 1024), RESAMPLE)
    plate = Image.new("RGBA", (1024, 1024))
    draw = ImageDraw.Draw(plate)
    for y in range(1024):
        ratio = min(1, max(0, (y - 100) / 824))
        draw.line((0, y, 1024, y), fill=tuple(round(a + (b - a) * ratio)
                   for a, b in zip((250, 252, 254), (231, 241, 250))))
    plate.putalpha(mask)
    plate.alpha_composite(fitted(logo, (MAC_ARTWORK_SIZE, MAC_ARTWORK_SIZE)))
    return channel_label(plate, channel, 861)


def windows_icon(logo, channel):
    # Transparent full-colour symbol with a safe edge for Explorer/taskbar scaling.
    image = fitted(logo, (924, 924))
    return channel_label(image, channel, 951)


def tray_master(logo):
    # Retain the recognizable navy hood/orbit/eyes as an alpha template; the pale
    # face and eye whites are negative space, not a solid black head silhouette.
    mask = []
    for r, g, b, alpha in logo.get_flattened_data():
        high, low = max(r, g, b), min(r, g, b)
        chroma = (high - low) / max(1, high)
        signal = max((chroma - .12) / .10, (150 - high) / 24)
        mask.append(round(alpha * min(1, max(0, signal))))
    image = Image.new("RGBA", logo.size)
    alpha = Image.new("L", logo.size)
    alpha.putdata(mask)
    image.putalpha(alpha)
    return fitted(image, (164, 164), (192, 192))


def save_icns(image, path):
    # Include16px1x and2x explicitly; Pillow's default ICNS omits16px1x.
    formats = ((b"icp4", 16), (b"ic11", 32), (b"icp5", 32), (b"ic12", 64),
               (b"ic07", 128), (b"ic13", 256), (b"ic08", 256), (b"ic14", 512),
               (b"ic09", 512), (b"ic10", 1024))
    entries = []
    for tag, size in formats:
        buffer = io.BytesIO()
        image.resize((size, size), RESAMPLE).save(buffer, "PNG")
        data = buffer.getvalue()
        entries.append(tag + struct.pack(">I", len(data) + 8) + data)
    data = b"".join(entries)
    path.write_bytes(b"icns" + struct.pack(">I", len(data) + 8) + data)


def save_icons(logo, folder, channel):
    folder.mkdir(parents=True, exist_ok=True)
    windows = windows_icon(logo, channel)
    macos = macos_icon(logo, channel)
    for size in PNG_SIZES:
        windows.resize((size, size), RESAMPLE).save(folder / f"{size}x{size}.png")
        target = folder / "macos" / f"{size}x{size}.png"
        target.parent.mkdir(parents=True, exist_ok=True)
        macos.resize((size, size), RESAMPLE).save(target)
    shutil.copyfile(folder / "256x256.png", folder / "128x128@2x.png")
    windows.save(folder / "icon.png")
    macos.save(folder / "macos/icon.png")
    save_icns(macos, folder / "icon.icns")
    windows.save(folder / "icon.ico", format="ICO", sizes=[(n, n) for n in ICO_SIZES])
    for size in WINDOWS_TILES:
        windows.resize((size, size), RESAMPLE).save(folder / f"Square{size}x{size}Logo.png")
    windows.resize((50, 50), RESAMPLE).save(folder / "StoreLogo.png")


def verify(folder):
    for channel in CHANNELS:
        path = folder if channel == "prod" else folder / channel
        with Image.open(path / "icon.ico") as image:
            if image.ico.sizes() != {(n, n) for n in ICO_SIZES}:
                raise ValueError("ICO is missing required Windows scale frames")
        with Image.open(path / "icon.icns") as image:
            sizes = image.info["sizes"]
            if not all(size in sizes for size in ((16, 16, 1), (16, 16, 2), (512, 512, 2))):
                raise ValueError("ICNS is missing16px or Retina frames")
            image.load()
        for subfolder in (path, path / "macos"):
            for size in PNG_SIZES:
                with Image.open(subfolder / f"{size}x{size}.png") as image:
                    if image.size != (size, size) or image.mode != "RGBA":
                        raise ValueError("PNG dimensions or alpha mode are invalid")
        with Image.open(path / "macos/icon.png") as image:
            if image.getpixel((512, 120))[3] != 255 or image.getpixel((0, 0))[3] != 0:
                raise ValueError("macOS icon needs an opaque rounded plate and transparent outer margin")
    for scale in (1, 2, 3):
        with Image.open(folder / f"tray-template@{scale}x.png") as image:
            if image.size != (24 * scale, 24 * scale):
                raise ValueError("Menu-bar scale is invalid")
            if any(r or g or b for r, g, b, _ in image.get_flattened_data()):
                raise ValueError("Tray template must be monochrome black with alpha")
            if image.getpixel((0, 0))[3] or image.getchannel("A").getextrema() != (0, 255):
                raise ValueError("Tray must retain transparent background and negative spaces")


def preview(folder):
    board = Image.new("RGB", (1440, 1040), "#f1f5fa")
    draw = ImageDraw.Draw(board)
    font = ImageFont.load_default(size=20)
    small = ImageFont.load_default(size=16)
    for column, channel in enumerate(CHANNELS):
        path = folder if channel == "prod" else folder / channel
        x = 24 + column * 474
        draw.text((x, 20), f"OSIR / {channel.upper()}", font=font, fill="#243c53")
        for row, (platform, name) in enumerate((("macOS / rounded plate", "macos/icon.png"),
                                                ("Windows / transparent", "icon.png"))):
            top = 58 + row * 340
            draw.text((x, top), platform, font=font, fill="#243c53")
            icon = Image.open(path / name)
            for dx, bg in ((0, "#ffffff"), (226, "#192b3d")):
                draw.rectangle((x + dx, top + 30, x + dx + 218, top + 248), fill=bg)
                thumb = icon.resize((208, 208), RESAMPLE)
                board.paste(thumb, (x + dx + 5, top + 35), thumb)
            offset = 0
            for size in (16, 32, 48, 64):
                thumb = icon.resize((size, size), RESAMPLE)
                board.paste(thumb, (x + offset, top + 265), thumb)
                draw.text((x + offset, top + 324), str(size), font=small, fill="#60788f")
                offset += size + 36
    draw.text((24, 756), "Shared Logo / sidebar40 / welcome64 / avatar56", font=font, fill="#243c53")
    logo = Image.open(folder / "brand-logo.png")
    for size, x in ((128, 26), (64, 185), (56, 290), (40, 390)):
        thumb = logo.resize((size, size), RESAMPLE)
        board.paste(thumb, (x, 800 + (128 - size) // 2), thumb)
        draw.text((x, 938), f"{size}px", font=small, fill="#60788f")
    draw.text((550, 756), "Tray / monochrome transparent / system tint", font=font, fill="#243c53")
    mask = Image.open(folder / "tray-template@2x.png").getchannel("A")
    for x, bg, tint in ((550, "#ffffff", "#000000"), (740, "#192b3d", "#ffffff")):
        draw.rectangle((x, 815, x + 160, 890), fill=bg)
        board.paste(Image.new("RGB", mask.size, tint), (x + 56, 830), mask)
    draw.text((24, 994), "macOS:824/1024 plate,704px character; Windows:924px character; tray:20.5/24pt visible mark", font=small, fill="#60788f")
    board.save(folder / "preview.png")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=SOURCES / "app-icon.png")
    parser.add_argument("--check", action="store_true", help="Verify all outputs without replacing files")
    args = parser.parse_args()
    logo = master(args.source)
    tray = tray_master(logo)
    with tempfile.TemporaryDirectory(prefix="osir-icons-") as temp:
        stage = Path(temp)
        for channel in CHANNELS:
            save_icons(logo, stage if channel == "prod" else stage / channel, channel)
        for scale in (1, 2, 3):
            tray.resize((24 * scale, 24 * scale), RESAMPLE).save(stage / f"tray-template@{scale}x.png")
        shutil.copyfile(stage / "tray-template@3x.png", stage / "tray-template.png")
        tray.save(stage / "tray-master.png")
        logo.save(stage / "brand-logo.png")
        logo.resize((128, 128), RESAMPLE).save(stage / "brand-logo-128.png")
        logo.resize((256, 256), RESAMPLE).save(stage / "brand-logo-256.png")
        verify(stage)
        preview(stage)
        files = sorted(p for p in stage.rglob("*") if p.is_file())
        manifest = {
            "schemaVersion": 2, "generator": "scripts/osir-icons.py", "pillowVersion": pillow_version,
            "source": {"app-icon.png": sha(args.source)},
            "outputs": {p.relative_to(stage).as_posix(): sha(p) for p in files},
            "uiIconSha256": sha(stage / "brand-logo-128.png"),
            "defaultAvatarSha256": sha(stage / "brand-logo-256.png"),
            "trayMasterSha256": sha(stage / "tray-master.png"), "windowsFrames": list(ICO_SIZES),
            "macOS": {"canvas": 1024, "plateBounds": list(MAC_PLATE), "artworkMax": MAC_ARTWORK_SIZE,
                      "corner": "continuous superellipse", "background": "subtle off-white/blue"},
            "menuBarPoints": 24, "menuBarScales": [1, 2, 3],
        }
        (stage / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        files.append(stage / "manifest.json")
        if args.check:
            mismatches = [str(p.relative_to(stage)) for p in files
                          if not (OUTPUT / p.relative_to(stage)).is_file()
                          or sha(p) != sha(OUTPUT / p.relative_to(stage))]
            if not UI_ICON.is_file() or sha(UI_ICON) != manifest["uiIconSha256"]:
                mismatches.append(str(UI_ICON.relative_to(ROOT)))
            if not DEFAULT_AVATAR.is_file() or sha(DEFAULT_AVATAR) != manifest["defaultAvatarSha256"]:
                mismatches.append(str(DEFAULT_AVATAR.relative_to(ROOT)))
            if not (SOURCES / "tray-master.png").is_file() or sha(SOURCES / "tray-master.png") != manifest["trayMasterSha256"]:
                mismatches.append("assets/brand/osir-client/tray-master.png")
            if mismatches:
                raise ValueError("Generated icons differ: " + ", ".join(mismatches))
        else:
            for path in files:
                target = OUTPUT / path.relative_to(stage)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, target)
            SOURCES.mkdir(parents=True, exist_ok=True)
            if args.source.resolve() != (SOURCES / "app-icon.png").resolve():
                shutil.copyfile(args.source, SOURCES / "app-icon.png")
            shutil.copyfile(stage / "tray-master.png", SOURCES / "tray-master.png")
            UI_ICON.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(stage / "brand-logo-128.png", UI_ICON)
            shutil.copyfile(stage / "brand-logo-256.png", DEFAULT_AVATAR)
    print("OSIR icons verified" if args.check else "OSIR Logo/macOS/Windows/tray icons generated for prod, Beta and Dev")


if __name__ == "__main__":
    main()
