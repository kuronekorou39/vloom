// 受信の復元 (fountain 符号の復号) を回すワーカー。アプリ版 receive/decode_worker.dart と同じ役割。
//
// 1 ブロックの復元計算は受信の最後にまとまって走り、主スレッドでやるとその間カメラの
// 画像も進捗の表示も止まる (アプリ版で 30MB の受信が 99% で止まって完了しなかった)。
// デコーダはこのワーカーの中だけに置き、パケットは投げっぱなしで受け取る。
// 復元できたら CRC-32 (wrap_payload) の検証まで済ませて中身を返す。
//
//   受け取る: { k: "init", oti } / { k: "feed", packets: Uint8Array[] }
//   返す:     { k: "ready" } / { k: "error", message } (不正な OTI) / { k: "fatal", message } (wasm を
//             読めない) / { k: "done", payload } / { k: "corrupt" } (CRC 不一致)

import init, { FountainDecoder, vcodeUnwrapPayload } from "./pkg/vloom_core_wasm.js";

let dec = null;
let finished = false;
// wasm の読み込みに失敗したら知らせる (知らせないと、呼び出し側は「仕上げ中」のまま待ち続ける)
const ready = init().then(
  () => true,
  (err) => {
    self.postMessage({ k: "fatal", message: String(err) });
    return false;
  },
);

self.onmessage = async (e) => {
  const m = e.data;
  if (!(await ready)) return;
  if (m.k === "init") {
    try {
      dec = new FountainDecoder(m.oti);
      self.postMessage({ k: "ready" });
    } catch (err) {
      self.postMessage({ k: "error", message: String(err) });
    }
    return;
  }
  if (m.k !== "feed" || !dec || finished) return;
  for (const pkt of m.packets) {
    if (!dec.addPacket(pkt)) continue;
    finished = true;
    // エンドツーエンド CRC-32 検証。不一致 = ゴミパケットが RaptorQ を汚染して復元結果が破損
    const payload = vcodeUnwrapPayload(dec.payload());
    if (payload) self.postMessage({ k: "done", payload }, [payload.buffer]);
    else self.postMessage({ k: "corrupt" });
    break;
  }
};
