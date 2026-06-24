"""ugg Irodori-TTS サイドカー (M4c Phase D)。

OpenAI 互換の HTTP サーバを `127.0.0.1` で起動し、ugg 本体 (Rust) から音声合成 /
参照音声生成を受け付ける。本ファイルは **モックモード** を初期実装として持ち、
Phase G で実 Aratako/Irodori-TTS モデルへの結線を行う。

CLI:
    python sidecar.py \\
        --asset-dir <path>          ugg の %APPDATA%\\ugg\\irodori\\ ルート
        --ready-file <path>         起動完了時に書き出す JSON (port, pid)
        --host 127.0.0.1            (省略可)
        --port 0                    0 で動的割当 (省略可)
        --mock                      実モデルを使わずモック wav を返す

エンドポイント (architecture §8.5):
    GET  /health                       → {status, gpu, mock}
    POST /v1/audio/speech              → OpenAI 互換 (wav バイナリ返却)
    POST /v1/voice_ref/generate        → キャプション → 参照音声 wav を out_path に保存
    POST /shutdown                     → 100ms 後にプロセス終了

設計判断:
- 動的ポート: `socket.bind(("127.0.0.1", 0))` で空きを取得し uvicorn `port` に渡す。
  uvicorn 自身が socket を作り直すので競合の余地がわずかに残るが、loopback でかつ
  すぐに bind するので実用上は問題なし。確実性が要れば SO_REUSEADDR + listen 済 socket
  を uvicorn に渡す API も検討可 (Phase E)。
- モック wav: numpy / wave (stdlib) で 22050Hz mono 16-bit。fastapi / uvicorn 以外の
  追加 pip 依存を増やさない。

このファイルは tauri.conf.json の bundle.resources に登録され、起動時に
`%APPDATA%\\ugg\\irodori\\sidecar.py` にコピーされてから ugg が `python.exe` で起動する。
"""

from __future__ import annotations

import argparse
import asyncio
import io
import json
import logging
import math
import os
import socket
import struct
import sys
import wave
from pathlib import Path
from typing import Optional

try:
    from fastapi import BackgroundTasks, FastAPI, HTTPException
    from fastapi.responses import JSONResponse, Response
    from pydantic import BaseModel, Field
    import uvicorn
except ImportError as exc:  # pragma: no cover - 起動失敗時にユーザーに見せる
    sys.stderr.write(
        f"sidecar.py: 必要な Python 依存がありません ({exc}). "
        "ugg の Irodori 資産 DL (M4c Phase C) を完了してから再試行してください。\n"
    )
    sys.exit(2)

LOG = logging.getLogger("ugg.irodori")
SAMPLE_RATE = 22050  # モック wav のサンプルレート

# M4c Phase G: 実モデルの HF モデル ID (architecture §8.3)。
# 実機検証時に Aratako/Irodori-TTS の最新サンプルを見ながら from_pretrained 経路を確定する。
MODEL_REPO_SYNTH = "Aratako/Irodori-TTS-500M-v3"
MODEL_REPO_VOICE_DESIGN = "Aratako/Irodori-TTS-500M-v2-VoiceDesign"
MODEL_REPO_CODEC = "Aratako/Semantic-DACVAE-Japanese-32dim"


# --- リクエスト型 ----------------------------------------------------------

class SpeechRequest(BaseModel):
    """OpenAI `POST /v1/audio/speech` 互換 (architecture §8.5)。"""

    model: str = Field(..., description="モデル名 (mock では未使用)")
    input: str = Field(..., description="合成するテキスト (preprocess 済み想定)")
    voice: str = Field(..., description="参照音声 ID または絶対パス")
    response_format: str = Field("wav", description="現状 wav のみサポート")
    speed: float = Field(1.0, ge=0.25, le=4.0)


class VoiceRefGenerateRequest(BaseModel):
    """`POST /v1/voice_ref/generate`。"""

    caption: str = Field(..., min_length=1, description="自然言語の声質指示")
    out_path: str = Field(..., description="生成 wav の保存先 (絶対パス)")


# --- モック wav 生成 -------------------------------------------------------

def make_mock_speech_wav(text: str, speed: float) -> bytes:
    """テキスト長に比例した正弦波 (440Hz) を 16-bit PCM mono で返す。

    1 文字あたり 80ms、最低 200ms、最大 8 秒で頭打ち。speed で長さを按分する。
    """
    base_ms = max(200, min(8000, len(text) * 80))
    duration_s = max(0.05, base_ms / 1000.0 / max(0.25, speed))
    n_samples = int(SAMPLE_RATE * duration_s)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        frames = bytearray()
        amp = 8000  # 16-bit 範囲の控えめな振幅
        for n in range(n_samples):
            sample = int(amp * math.sin(2 * math.pi * 440.0 * n / SAMPLE_RATE))
            frames.extend(struct.pack("<h", sample))
        w.writeframes(bytes(frames))
    return buf.getvalue()


def make_mock_voice_ref_wav() -> bytes:
    """1 秒の無音 16-bit PCM mono wav。"""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        w.writeframes(b"\x00\x00" * SAMPLE_RATE)
    return buf.getvalue()


# --- HF モデル DL + 実モデル推論 (M4c Phase G, 実機検証で確定) -----------

def download_models(asset_dir: Path) -> None:
    """Aratako/Irodori-TTS 系モデルを `asset_dir/model/<repo>` に取得する。

    既に揃っていれば何もしない。stderr に進捗を出力し、Rust 側 (sidecar の child stderr) で listen 可能。
    実機検証 (Phase G の DoD) で完了確認する。
    """
    try:
        from huggingface_hub import snapshot_download  # type: ignore
    except ImportError:
        sys.stderr.write(
            "sidecar.py: huggingface_hub が見つかりません。Irodori 資産 DL を実行してください\n"
        )
        raise
    target_root = asset_dir / "model"
    target_root.mkdir(parents=True, exist_ok=True)
    repos = [MODEL_REPO_SYNTH, MODEL_REPO_VOICE_DESIGN, MODEL_REPO_CODEC]
    for repo in repos:
        local_dir = target_root / repo.replace("/", "__")
        if local_dir.is_dir() and any(local_dir.iterdir()):
            sys.stderr.write(f"sidecar.py: {repo} は既に取得済み (skip)\n")
            continue
        sys.stderr.write(f"sidecar.py: {repo} をダウンロード中…\n")
        snapshot_download(
            repo_id=repo,
            local_dir=str(local_dir),
            local_dir_use_symlinks=False,
        )
        sys.stderr.write(f"sidecar.py: {repo} ダウンロード完了\n")


class RealModelBackend:
    """実 Aratako/Irodori-TTS を用いた推論の薄いラッパ (Phase G の TODO)。

    実機検証時に Aratako/Irodori-TTS リポジトリの最新サンプルを参照しながら
    `from_pretrained` / `synthesize(text, reference_audio)` / VoiceDesign の API を確定する。
    本クラスは現状「未実装」を返すスタブで、サイドカー起動経路だけを通す。
    """

    def __init__(self, asset_dir: Path) -> None:
        self.asset_dir = asset_dir
        self.synth = None
        self.voice_design = None
        # TODO(Phase G 実機): 例えば下記のような実装になる見込み:
        #   from irodori_tts import IrodoriSynth, VoiceDesigner
        #   self.synth = IrodoriSynth.from_pretrained(asset_dir / "model" / MODEL_REPO_SYNTH.replace("/", "__"))
        #   self.voice_design = VoiceDesigner.from_pretrained(
        #       asset_dir / "model" / MODEL_REPO_VOICE_DESIGN.replace("/", "__"))
        # ※ 実 API は Aratako/Irodori-TTS の README / examples を参照して確定する。

    def synthesize(self, text: str, voice_ref_path: Path, speed: float) -> bytes:
        # TODO(Phase G 実機): self.synth(text=text, reference_audio=str(voice_ref_path), speed=speed)
        #   → 返り値の numpy array (sr=...) を soundfile.write で wav バイト列に変換
        raise NotImplementedError(
            "実 Irodori 推論は M4c Phase G の実機検証で実装します"
        )

    def generate_voice_ref(self, caption: str, out_path: Path) -> None:
        # TODO(Phase G 実機): self.voice_design(caption=caption) → wav → out_path
        raise NotImplementedError(
            "実 VoiceDesign は M4c Phase G の実機検証で実装します"
        )


# --- FastAPI アプリ --------------------------------------------------------

def build_app(asset_dir: Path, mock: bool, backend: Optional[RealModelBackend]) -> FastAPI:
    app = FastAPI(title="ugg-irodori-sidecar", docs_url=None, redoc_url=None)

    @app.get("/health")
    async def health() -> JSONResponse:
        gpu_name: Optional[str] = None
        if not mock:
            try:
                import torch  # type: ignore
                if torch.cuda.is_available():
                    gpu_name = torch.cuda.get_device_name(0)
            except Exception:
                gpu_name = None
        return JSONResponse(
            {"status": "ok", "gpu": gpu_name, "mock": mock}
        )

    @app.post("/v1/audio/speech")
    async def speech(req: SpeechRequest) -> Response:
        if req.response_format != "wav":
            raise HTTPException(415, f"未対応の response_format: {req.response_format}")
        if mock or backend is None:
            wav_bytes = make_mock_speech_wav(req.input, req.speed)
        else:
            try:
                wav_bytes = backend.synthesize(
                    text=req.input,
                    voice_ref_path=Path(req.voice),
                    speed=req.speed,
                )
            except NotImplementedError as exc:
                raise HTTPException(501, str(exc))
            except Exception as exc:
                raise HTTPException(500, f"Irodori 合成失敗: {exc}")
        return Response(content=wav_bytes, media_type="audio/wav")

    @app.post("/v1/voice_ref/generate")
    async def voice_ref_generate(req: VoiceRefGenerateRequest) -> JSONResponse:
        out = Path(req.out_path)
        try:
            out.parent.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            raise HTTPException(500, f"出力先の作成に失敗: {exc}")
        if mock or backend is None:
            wav = make_mock_voice_ref_wav()
            try:
                out.write_bytes(wav)
            except OSError as exc:
                raise HTTPException(500, f"wav 書き込みに失敗: {exc}")
        else:
            try:
                backend.generate_voice_ref(caption=req.caption, out_path=out)
            except NotImplementedError as exc:
                raise HTTPException(501, str(exc))
            except Exception as exc:
                raise HTTPException(500, f"VoiceDesign 失敗: {exc}")
        return JSONResponse({"status": "ok", "path": str(out.resolve())})

    @app.post("/shutdown")
    async def shutdown(bg: BackgroundTasks) -> JSONResponse:
        async def _exit() -> None:
            await asyncio.sleep(0.1)
            # uvicorn の signal handler を介さず即終了
            os._exit(0)

        bg.add_task(_exit)
        return JSONResponse({"status": "ok"})

    return app


# --- ready.json 書き出し + uvicorn 起動 ------------------------------------

def pick_free_port(host: str) -> int:
    """OS にバインドして即解放した空きポート番号を返す。"""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind((host, 0))
        return s.getsockname()[1]


def write_ready_file(path: Path, port: int) -> None:
    payload = {"port": port, "pid": os.getpid()}
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload), encoding="utf-8")
    tmp.replace(path)


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="ugg Irodori-TTS sidecar")
    parser.add_argument("--asset-dir", required=True, type=Path)
    parser.add_argument("--ready-file", required=True, type=Path)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0, help="0 で動的割当")
    parser.add_argument("--mock", action="store_true", help="実モデルを使わず正弦波 wav を返す")
    parser.add_argument(
        "--no-download",
        action="store_true",
        help="起動時の HF モデル DL を skip する (デバッグ / 事前 DL 済 用)",
    )
    parser.add_argument("--log-level", default="warning")
    args = parser.parse_args(argv)

    logging.basicConfig(level=args.log_level.upper())
    asset_dir: Path = args.asset_dir
    asset_dir.mkdir(parents=True, exist_ok=True)

    port = args.port if args.port and args.port > 0 else pick_free_port(args.host)
    LOG.info("sidecar binding to %s:%d (mock=%s)", args.host, port, args.mock)

    backend: Optional[RealModelBackend] = None
    if not args.mock:
        if not args.no_download:
            try:
                download_models(asset_dir)
            except Exception as exc:
                sys.stderr.write(f"sidecar.py: モデル DL 失敗: {exc}\n")
                return 1
        try:
            backend = RealModelBackend(asset_dir)
        except Exception as exc:
            sys.stderr.write(f"sidecar.py: backend 初期化失敗: {exc}\n")
            return 1

    app = build_app(asset_dir=asset_dir, mock=args.mock, backend=backend)

    try:
        write_ready_file(args.ready_file, port)
    except OSError as exc:
        sys.stderr.write(f"sidecar.py: ready file 書き出し失敗: {exc}\n")
        return 1

    try:
        uvicorn.run(
            app,
            host=args.host,
            port=port,
            log_level=args.log_level,
            access_log=False,
        )
    except SystemExit:
        raise
    except Exception as exc:  # pragma: no cover
        sys.stderr.write(f"sidecar.py: uvicorn 異常終了: {exc}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
