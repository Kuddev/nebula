import binascii
import json
import math
import re
import struct
import unittest
import xml.etree.ElementTree as ET
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SIZES = (16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 256)
VARIANTS = ("light", "dark", "titanium")
NAMESPACE = {"svg": "http://www.w3.org/2000/svg"}


def ico_frames(path):
    data = path.read_bytes()
    reserved, kind, count = struct.unpack_from("<HHH", data)
    assert (reserved, kind, count) == (0, 1, len(SIZES))
    offset_expected = 6 + 16 * count
    frames = {}
    for index in range(count):
        width, height, colors, reserved, planes, depth, length, offset = struct.unpack_from(
            "<BBBBHHII", data, 6 + index * 16
        )
        assert width == height
        assert (colors, reserved, planes, depth) == (0, 0, 1, 32)
        assert offset == offset_expected
        frame = data[offset:offset + length]
        assert len(frame) == length
        size = width or 256
        assert size not in frames
        frames[size] = frame
        offset_expected += length
    assert offset_expected == len(data)
    assert tuple(frames) == SIZES
    return frames


def decode_png(data):
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    offset = 8
    image_data = []
    while offset < len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4:offset + 8]
        payload = data[offset + 8:offset + 8 + length]
        checksum = struct.unpack_from(">I", data, offset + 8 + length)[0]
        assert binascii.crc32(kind + payload) == checksum
        if kind == b"IHDR":
            width, height, depth, color, compression, filtering, interlace = struct.unpack(
                ">IIBBBBB", payload
            )
            assert (depth, color, compression, filtering, interlace) == (8, 6, 0, 0, 0)
        elif kind == b"IDAT":
            image_data.append(payload)
        offset += length + 12
    raw = zlib.decompress(b"".join(image_data))
    stride = width * 4
    assert len(raw) == height * (stride + 1)
    previous = bytearray(stride)
    rows = []
    for row_index in range(height):
        start = row_index * (stride + 1)
        filtering = raw[start]
        assert filtering in range(5)
        row = bytearray(raw[start + 1:start + 1 + stride])
        for column in range(stride):
            left = row[column - 4] if column >= 4 else 0
            above = previous[column]
            upper_left = previous[column - 4] if column >= 4 else 0
            predictor = left + above - upper_left
            distances = [abs(predictor - value) for value in (left, above, upper_left)]
            paeth = (left, above, upper_left)[distances.index(min(distances))]
            row[column] = (row[column] + (0, left, above, (left + above) // 2, paeth)[filtering]) % 256
        rows.append([tuple(row[column:column + 4]) for column in range(0, stride, 4)])
        previous = row
    return width, height, rows


def color(hex_value):
    return tuple(int(hex_value[index:index + 2], 16) for index in (1, 3, 5))


def contrast(first, second):
    def luminance(channels):
        linear = [value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4
                  for value in (channel / 255 for channel in channels)]
        return sum(channel * weight for channel, weight in zip(linear, (0.2126, 0.7152, 0.0722)))
    brighter, darker = sorted((luminance(first), luminance(second)), reverse=True)
    return (brighter + 0.05) / (darker + 0.05)


class AppIconTests(unittest.TestCase):
    def test_shared_resource_budget_and_titanium_default(self):
        primary = ROOT / "nebula_app/windows/nebula.ico"
        coverage = ROOT / "extra/logo/nebula-coverage.png"
        self.assertLessEqual(primary.stat().st_size + coverage.stat().st_size, 96 * 1024)
        self.assertEqual((ROOT / "nebula_app/windows/nebula-titanium.ico").read_bytes(), primary.read_bytes())
        self.assertEqual((ROOT / "extra/logo/nebula.png").read_bytes(),
                         (ROOT / "extra/logo/nebula-titanium.png").read_bytes())
        resource = (ROOT / "nebula_app/windows/nebula.rc").read_text()
        self.assertEqual(resource.count(' ICON "'), 1)
        for variant in VARIANTS:
            self.assertNotIn(f'"nebula-{variant}.ico"', resource)

    def test_catalog_preserves_all_24_color_lab_palettes(self):
        catalog = json.loads((ROOT / "extra/logo/icon-catalog.json").read_text())
        self.assertEqual(catalog["default"], "titanium")
        self.assertEqual(tuple(catalog["sizes"]), SIZES)
        entries = {palette["number"]: palette for palette in catalog["palettes"]}
        self.assertEqual(set(entries), {f"{number:02}" for number in range(1, 26)})
        self.assertEqual(len({palette["key"] for palette in entries.values()}), 25)
        original_colors = (
            "EEF4FC 234B85 93AEC9", "DCEBFF 2353BA 839DC8", "275DDF F3F7FF 1C45AC",
            "101D3A DBEAFE 647DAA", "CBD8E8 334A68 879BAF", "4668CF F5F7FF 3152B2",
            "F5F7FC 1D4ED8 ADBBD7", "101827 8BB9FF 5578A6", "EDE9FE 6640B4 AD9BCD",
            "6C47C9 F6F0FF 5235A2", "251D3C D9C7FF 78668F", "E6E7F4 585B89 AAACC3",
            "DEF6FA 0C657A 79ABB7", "087E9A ECFDFF 085E76", "102B36 9FE5EE 4D8B97",
            "DAF5E9 17644E 81B9A4", "116953 ECFFF5 0B4C3E", "112A28 A5EDD6 527F76",
            "F4F5F7 252A33 A5ACB7", "232730 F1F3F7 717984", "D9DFE6 374151 909BA9",
            "111318 FFFFFF 737982", "E4EBDD 263A32 819581", "18231F E8EFE2 819581",
        )
        for number, colors in enumerate(original_colors, 1):
            self.assertEqual(tuple(entries[f"{number:02}"][key] for key in ("tile", "mark", "border")),
                             tuple(f"#{value}" for value in colors.split()))
        source = (ROOT / "nebula_settings/src/app_icon.rs").read_text()
        canonical = re.findall(
            r'\w+\s*=>\s*\(\s*"([^"]+)"\s*,\s*"(\d+)"\s*,\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,'
            r'\s*0x([\dA-Fa-f]{6})\s*,\s*0x([\dA-Fa-f]{6})\s*,\s*0x([\dA-Fa-f]{6})\s*\)', source)
        self.assertEqual(len(canonical), 25)
        for key, number, name_zh, name_en, tile, mark, border in canonical:
            self.assertEqual(entries[number], dict(key=key, number=number, nameZh=name_zh,
                             nameEn=name_en, tile=f"#{tile}", mark=f"#{mark}", border=f"#{border}"))
        for palette in entries.values():
            self.assertGreaterEqual(contrast(color(palette["tile"]), color(palette["mark"])), 4.0)

    def test_coverage_atlas_preserves_premultiplied_edge_weights(self):
        width, height, rows = decode_png((ROOT / "extra/logo/nebula-coverage.png").read_bytes())
        self.assertEqual((width, height), (512, sum(SIZES) + 512))
        partial = 0
        for row in rows:
            for tile, border, mark, alpha in row:
                self.assertLessEqual(max(tile, border, mark), alpha)
                self.assertLessEqual(abs(tile + border + mark - alpha), 2)
                if alpha == 0:
                    self.assertEqual((tile, border, mark), (0, 0, 0))
                elif alpha < 255:
                    partial += 1
        self.assertGreater(partial, 1000)

    def test_geometry_and_solid_color_contrast(self):
        shapes = []
        for variant in VARIANTS:
            source = ET.parse(ROOT / "extra/logo" / f"nebula-{variant}.svg").getroot()
            paths = source.findall(".//svg:path", NAMESPACE)
            shapes.append([path.get("d") for path in paths])
            tile = source.find("svg:rect", NAMESPACE)
            mark = source.find("svg:use", NAMESPACE)
            self.assertEqual((tile.get("x"), tile.get("width"), tile.get("rx")), ("5", "118", "32"))
            self.assertEqual((mark.get("x"), mark.get("width")), ("19", "90"))
            self.assertGreaterEqual(contrast(color(tile.get("fill")), color(mark.get("color"))), 4.5)
        self.assertTrue(all(shape == shapes[0] for shape in shapes))

    def test_native_frames_transparency_size_and_prompt_separation(self):
        for variant in VARIANTS:
            source = ET.parse(ROOT / "extra/logo" / f"nebula-{variant}.svg").getroot()
            tile = color(source.find("svg:rect", NAMESPACE).get("fill"))
            mark = color(source.find("svg:use", NAMESPACE).get("color"))
            for size, frame in ico_frames(ROOT / "nebula_app/windows" / f"nebula-{variant}.ico").items():
                with self.subTest(variant=variant, size=size):
                    width, height, pixels = decode_png(frame)
                    self.assertEqual((width, height), (size, size))
                    self.assertEqual(pixels[0][0][3], 0)
                    occupied = [(column, row) for row in range(size) for column in range(size)
                                if pixels[row][column][3] > 128]
                    self.assertGreaterEqual(max(column for column, _ in occupied) - min(column for column, _ in occupied) + 1, size * 0.85)
                    strength = max(0, min(1, (64 - size) / 40))
                    extent = 90 + 18 * strength
                    scale, origin = extent / 128, (128 - extent) / 2
                    def samples(local_x, local_y):
                        target_x = (origin + scale * local_x) * size / 128 - 0.5
                        target_y = (origin + scale * local_y) * size / 128 - 0.5
                        return [pixels[row][column][:3]
                                for row in {math.floor(target_y), math.ceil(target_y)}
                                for column in {math.floor(target_x), math.ceil(target_x)}]
                    def distance(first, second):
                        return sum((left - right) ** 2 for left, right in zip(first, second))
                    for prompt_point in [(46, 59), (46, 71), (77, 80)]:
                        self.assertTrue(any(distance(sample, tile) < distance(sample, mark)
                                            for sample in samples(*prompt_point)))
                    self.assertTrue(any(distance(sample, mark) < distance(sample, tile)
                                        for sample in samples(62, 79)))

    def test_taskbar_frames_enlarge_the_mark_and_keep_a_solid_underscore(self):
        _, _, atlas = decode_png((ROOT / "extra/logo/nebula-coverage.png").read_bytes())
        for size in SIZES:
            if size >= 64:
                continue
            with self.subTest(size=size):
                top = sum(stored for stored in SIZES if stored < size)
                pixels = [row[:size] for row in atlas[top:top + size]]
                mark_columns = [column for row in pixels for column, pixel in enumerate(row) if pixel[2] > 128]
                extent = 90 + 18 * min(1, (64 - size) / 40)
                expected_width = 96 * extent * size / (128 * 128)
                self.assertAlmostEqual(max(mark_columns) - min(mark_columns) + 1, expected_width, delta=1)
                scale = extent * size / (128 * 128)
                offset = (128 - extent) / 2 * size / 128
                center_x, center_y = offset + 77 * scale, offset + 80 * scale
                self.assertTrue(any(pixels[row][column][0] >= 224
                                    for row in {math.floor(center_y - 0.5), math.ceil(center_y - 0.5)}
                                    for column in {math.floor(center_x - 0.5), math.ceil(center_x - 0.5)}))


if __name__ == "__main__":
    unittest.main()
