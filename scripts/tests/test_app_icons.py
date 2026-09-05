import binascii
import math
import struct
import unittest
import xml.etree.ElementTree as ET
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)
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
    def test_curated_resource_budget_and_default(self):
        paths = [ROOT / "nebula_app/windows" / f"nebula-{variant}.ico" for variant in VARIANTS]
        self.assertLessEqual(sum(path.stat().st_size for path in paths), 64 * 1024)
        self.assertEqual(paths[0].read_bytes(), (ROOT / "nebula_app/windows/nebula.ico").read_bytes())
        self.assertEqual((ROOT / "extra/logo/nebula.png").read_bytes(),
                         (ROOT / "extra/logo/nebula-light.png").read_bytes())
        resource = (ROOT / "nebula_app/windows/nebula.rc").read_text()
        self.assertEqual(resource.count(' ICON "'), 3)
        self.assertNotIn('"nebula-light.ico"', resource)

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
                    self.assertGreaterEqual(max(column for column, _ in occupied) - min(column for column, _ in occupied), size * 0.85)
                    scale, origin = (100 / 128, 14) if size <= 24 else (90 / 128, 19)
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


if __name__ == "__main__":
    unittest.main()
