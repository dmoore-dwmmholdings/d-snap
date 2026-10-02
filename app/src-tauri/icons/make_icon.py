"""Draw the D-Snap app icon (a camera-shutter mark) as icon-source.png.

Regenerate every size with:  python make_icon.py && npx tauri icon icon-source.png
(run in app/src-tauri/icons, then move the generated files here).
"""
import math

from PIL import Image, ImageDraw

SIZE = 1024
SCALE = 4  # draw large, then downsample for smooth edges
S = SIZE * SCALE
C = S / 2

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# Rounded-square background.
pad = S * 0.06
d.rounded_rectangle([pad, pad, S - pad, S - pad], radius=S * 0.2, fill=(36, 99, 235, 255))

# Aperture: six blades as triangles fanning around a hexagonal hole.
r_out = S * 0.33
r_in = S * 0.12
blades = 6
colors = [(255, 255, 255, 255), (219, 230, 255, 255)]
for i in range(blades):
    a0 = math.radians(i * 360 / blades - 90)
    a1 = math.radians((i + 1) * 360 / blades - 90)
    tip = (C + r_in * math.cos(a0 + math.radians(30)), C + r_in * math.sin(a0 + math.radians(30)))
    p0 = (C + r_out * math.cos(a0), C + r_out * math.sin(a0))
    p1 = (C + r_out * math.cos(a1), C + r_out * math.sin(a1))
    d.polygon([p0, p1, tip], fill=colors[i % 2])

# Outer ring and the dark hole in the middle.
w = S * 0.03
d.ellipse([C - r_out - w, C - r_out - w, C + r_out + w, C + r_out + w], outline=(255, 255, 255, 255), width=int(w))
hole = [
    (C + r_in * 0.95 * math.cos(math.radians(k * 60)), C + r_in * 0.95 * math.sin(math.radians(k * 60)))
    for k in range(6)
]
d.polygon(hole, fill=(16, 42, 107, 255))

img.resize((SIZE, SIZE), Image.LANCZOS).save("icon-source.png")
