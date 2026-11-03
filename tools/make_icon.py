"""Ayna Merkezi logosu: assets/icon.ico (16–256 px) ve exe'ye gömülecek assets/icon.res.

Logo: yuvarlak köşeli mavi→mor zemin; arkada bir Mac ekranı, önde bir iPhone. Pillow ile 1024 px'te çizilip küçültülür.

Kullanım: python -I tools/make_icon.py
"""
import io
import os
import struct

from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "assets")
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
N = 1024


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))


def draw_logo():
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    # Zemin: çapraz gradyan, yuvarlak köşeli kare.
    grad = Image.new("RGBA", (N, N))
    top, bottom = (10, 132, 255, 255), (94, 92, 230, 255)
    gp = grad.load()
    for y in range(N):
        for x in range(N):
            gp[x, y] = lerp(top, bottom, (x + y) / (2 * N))
    mask = Image.new("L", (N, N), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, N - 1, N - 1], radius=230, fill=255)
    img.paste(grad, (0, 0), mask)

    white = (255, 255, 255, 255)
    # Mac ekranı ayrı katmanda çizilir; iPhone'un çevresi bu katmandan oyulur (arkası gerçekten boş).
    mac = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    m = ImageDraw.Draw(mac)
    m.rounded_rectangle([140, 230, 720, 610], radius=44, outline=white, width=46)
    m.rectangle([375, 610, 485, 700], fill=white)
    m.rounded_rectangle([285, 690, 575, 736], radius=23, fill=white)
    phone = [590, 360, 880, 860]
    hole = Image.new("L", (N, N), 255)
    ImageDraw.Draw(hole).rounded_rectangle([phone[0] - 34, phone[1] - 34, phone[2] + 34, phone[3] + 34], radius=96, fill=0)
    mac.putalpha(Image.composite(mac.getchannel("A"), Image.new("L", (N, N), 0), hole))
    img.alpha_composite(mac)
    d = ImageDraw.Draw(img)
    # iPhone (önde): çerçeve ve çentik.
    d.rounded_rectangle(phone, radius=66, outline=white, width=42)
    d.rounded_rectangle([690, 404, 780, 432], radius=14, fill=white)
    return img


def ico_and_res(img):
    images = []
    for s in SIZES:
        im = img.resize((s, s), Image.LANCZOS)
        buf = io.BytesIO()
        im.save(buf, format="PNG")
        images.append((s, buf.getvalue()))

    # .ico
    ico = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    data = b""
    for s, png in images:
        b = 0 if s >= 256 else s
        ico += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(png), offset + len(data))
        data += png
    ico += data

    # .res (Win32 kaynak dosyası): RT_ICON (3) her boyut için, RT_GROUP_ICON (14) ad 1.
    def entry(rtype, name, payload, flags):
        header = struct.pack("<II", len(payload), 32)
        header += struct.pack("<HH", 0xFFFF, rtype) + struct.pack("<HH", 0xFFFF, name)
        header += struct.pack("<IHHII", 0, flags, 0x0409, 0, 0)
        pad = (-len(payload)) % 4
        return header + payload + b"\0" * pad

    res = struct.pack("<II", 0, 32) + struct.pack("<HHHH", 0xFFFF, 0, 0xFFFF, 0) + struct.pack("<IHHII", 0, 0, 0, 0, 0)
    group = struct.pack("<HHH", 0, 1, len(images))
    for i, (s, png) in enumerate(images, start=1):
        res += entry(3, i, png, 0x1010)
        b = 0 if s >= 256 else s
        group += struct.pack("<BBBBHHIH", b, b, 0, 0, 1, 32, len(png), i)
    res += entry(14, 1, group, 0x1030)
    return ico, res


def main():
    os.makedirs(OUT, exist_ok=True)
    img = draw_logo()
    ico, res = ico_and_res(img)
    img.resize((512, 512), Image.LANCZOS).save(os.path.join(OUT, "logo.png"))
    with open(os.path.join(OUT, "icon.ico"), "wb") as f:
        f.write(ico)
    with open(os.path.join(OUT, "icon.res"), "wb") as f:
        f.write(res)
    print("ok", len(ico), len(res))


if __name__ == "__main__":
    main()
