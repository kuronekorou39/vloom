"""tools/vcode_encode.py のペイロード組み立て (wrap_file / wrap_payload) を検証する。

受信側 (vcode::unwrap_file 相当) がこのバイト列をそのまま読むので、レイアウトが
1 バイトでもずれると実機の受信が壊れる。ここでは送信側の組み立てだけを、
仕様どおりに手でパースし直して確認する。
"""

from __future__ import annotations

import struct
import zlib

import vcode_encode as ve


def test_wrap_file_round_trip_fields():
    name, mime, data = "photo.jpg", "image/jpeg", b"\x01\x02\x03hello"
    wrapped = ve.wrap_file(name, mime, data)

    assert wrapped[:4] == ve.FILE_MAGIC
    assert wrapped[4] == 1  # バージョン
    offset = 5

    (name_len,) = struct.unpack_from("<H", wrapped, offset)
    offset += 2
    assert wrapped[offset:offset + name_len].decode("utf-8") == name
    offset += name_len

    (mime_len,) = struct.unpack_from("<H", wrapped, offset)
    offset += 2
    assert wrapped[offset:offset + mime_len].decode("utf-8") == mime
    offset += mime_len

    assert wrapped[offset:] == data


def test_wrap_payload_prepends_end_to_end_crc32():
    data = b"vloom test payload"
    payload = ve.wrap_payload(data)

    (crc,) = struct.unpack(">I", payload[:4])
    assert crc == zlib.crc32(data) & 0xFFFFFFFF
    assert payload[4:] == data
