from pathlib import Path

from PIL import Image, ImageFilter


ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "crates" / "audio-switch-windows" / "assets" / "tray"
ICON_NAMES = ("primary", "secondary", "warning")
CANVAS_SIZE = 256
PADDING = 18
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
OUTLINE_COLOR = (8, 27, 48, 255)
OUTLINE_FILTER_SIZE = 15


def build_icon(name: str) -> None:
    png_path = ICON_DIR / f"{name}.png"
    image = Image.open(png_path).convert("RGBA")
    alpha_bounds = image.getchannel("A").getbbox()
    if alpha_bounds is None:
        raise ValueError(f"{png_path} contains no visible pixels")

    subject = image.crop(alpha_bounds)
    available = CANVAS_SIZE - (PADDING * 2)
    scale = min(available / subject.width, available / subject.height)
    subject = subject.resize(
        (
            max(1, round(subject.width * scale)),
            max(1, round(subject.height * scale)),
        ),
        Image.Resampling.LANCZOS,
    )

    canvas = Image.new("RGBA", (CANVAS_SIZE, CANVAS_SIZE), (0, 0, 0, 0))
    canvas.alpha_composite(
        subject,
        (
            (CANVAS_SIZE - subject.width) // 2,
            (CANVAS_SIZE - subject.height) // 2,
        ),
    )
    outline_alpha = canvas.getchannel("A").filter(
        ImageFilter.MaxFilter(OUTLINE_FILTER_SIZE)
    )
    outlined = Image.new("RGBA", canvas.size, OUTLINE_COLOR)
    outlined.putalpha(outline_alpha)
    outlined.alpha_composite(canvas)
    canvas = outlined

    canvas.save(png_path, optimize=True)
    canvas.save(ICON_DIR / f"{name}.ico", sizes=[(size, size) for size in ICO_SIZES])


for icon_name in ICON_NAMES:
    build_icon(icon_name)
