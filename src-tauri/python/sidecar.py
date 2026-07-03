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

    既に揃っていれば何もしない。stderr に進捗を出力し、Rust 側 (sidecar の child stderr)
    が `[hf-download] ...` 行を pick して `irodori-download` イベントへ転送する。
    """
    try:
        from huggingface_hub import hf_hub_download, snapshot_download  # type: ignore
    except ImportError:
        sys.stderr.write(
            "[hf-download] huggingface_hub が見つかりません。Irodori 資産 DL を実行してください\n"
        )
        raise
    target_root = asset_dir / "model"
    target_root.mkdir(parents=True, exist_ok=True)

    # 合成 / VoiceDesign 本体は upstream infer.py と同じく `model.safetensors` 1 ファイルでよい
    # (config 情報は safetensors のメタデータに埋め込まれている)。
    weight_repos = [MODEL_REPO_SYNTH, MODEL_REPO_VOICE_DESIGN]
    for repo in weight_repos:
        local_dir = target_root / repo.replace("/", "__")
        weight_file = local_dir / "model.safetensors"
        if weight_file.is_file() and weight_file.stat().st_size > 0:
            sys.stderr.write(f"[hf-download] {repo} は既に取得済み (skip)\n")
            continue
        sys.stderr.write(f"[hf-download] {repo}/model.safetensors をダウンロード中…\n")
        local_dir.mkdir(parents=True, exist_ok=True)
        hf_hub_download(
            repo_id=repo,
            filename="model.safetensors",
            local_dir=str(local_dir),
            local_dir_use_symlinks=False,
        )
        sys.stderr.write(f"[hf-download] {repo} ダウンロード完了\n")

    # コーデック (DACVAE) は InferenceRuntime が repo_id 文字列でロードするので
    # HF cache に snapshot しておけば codec_repo 経由で読まれる。
    codec_dir = target_root / MODEL_REPO_CODEC.replace("/", "__")
    if codec_dir.is_dir() and any(codec_dir.iterdir()):
        sys.stderr.write(f"[hf-download] {MODEL_REPO_CODEC} は既に取得済み (skip)\n")
    else:
        sys.stderr.write(f"[hf-download] {MODEL_REPO_CODEC} をダウンロード中…\n")
        snapshot_download(
            repo_id=MODEL_REPO_CODEC,
            local_dir=str(codec_dir),
            local_dir_use_symlinks=False,
        )
        sys.stderr.write(f"[hf-download] {MODEL_REPO_CODEC} ダウンロード完了\n")


class RealModelBackend:
    """実 Aratako/Irodori-TTS を用いた推論の薄いラッパ。

    upstream `infer.py` (https://github.com/Aratako/Irodori-TTS/blob/main/infer.py) と
    同じ `irodori_tts.inference_runtime` API (`InferenceRuntime` / `SamplingRequest`) を
    利用する。モデルロードは初回 synthesize/generate_voice_ref まで遅延 (VRAM を起動時に
    取らない方針)。

    依存パッケージ (Phase C で導入される irodori-tts + dacvae + silentcipher + transformers
    系) が揃っていない環境で `RealModelBackend` を初期化しても問題ないよう、import は
    各メソッド内で行う。
    """

    # 合成テキストは preprocess (voicevox OpenJtalk) で読みやすい仮名列が渡される想定。
    # VoiceDesign の参照音声には固定の短いキャプション読みを使う。
    VOICE_REF_READING_TEXT = "こんにちは、これは参照音声です。"

    def __init__(self, asset_dir: Path) -> None:
        self.asset_dir = asset_dir
        self._synth_runtime = None
        self._voice_design_runtime = None

    @staticmethod
    def _resolve_device() -> str:
        try:
            import torch  # type: ignore

            return "cuda" if torch.cuda.is_available() else "cpu"
        except Exception:
            return "cpu"

    def _checkpoint_path(self, repo: str) -> Path:
        return self.asset_dir / "model" / repo.replace("/", "__") / "model.safetensors"

    def _build_runtime(self, repo: str):
        """upstream infer.py の InferenceRuntime.from_key(RuntimeKey(...)) と同じ構成。"""
        from irodori_tts.inference_runtime import (  # type: ignore
            InferenceRuntime,
            RuntimeKey,
        )

        ckpt = self._checkpoint_path(repo)
        if not ckpt.is_file():
            raise FileNotFoundError(
                f"model.safetensors が見つかりません: {ckpt}. download_models を先に実行してください"
            )
        device = self._resolve_device()
        return InferenceRuntime.from_key(
            RuntimeKey(
                checkpoint=str(ckpt),
                model_device=device,
                codec_repo=MODEL_REPO_CODEC,
                model_precision="fp32",
                codec_device=device,
                codec_precision="fp32",
                codec_deterministic_encode=True,
                codec_deterministic_decode=True,
                compile_model=False,
                compile_dynamic=False,
            )
        )

    def _load_synth(self):
        if self._synth_runtime is None:
            self._synth_runtime = self._build_runtime(MODEL_REPO_SYNTH)
        return self._synth_runtime

    def _load_voice_design(self):
        if self._voice_design_runtime is None:
            self._voice_design_runtime = self._build_runtime(MODEL_REPO_VOICE_DESIGN)
        return self._voice_design_runtime

    @staticmethod
    def _make_request(
        *,
        text: str,
        caption: Optional[str],
        ref_wav: Optional[str],
        no_ref: bool,
        duration_scale: float,
    ):
        """upstream infer.py のデフォルト引数群を写し取った SamplingRequest を組み立てる。"""
        from irodori_tts.inference_runtime import SamplingRequest  # type: ignore

        return SamplingRequest(
            text=text,
            caption=caption,
            ref_wav=ref_wav,
            ref_latent=None,
            ref_embed=None,
            no_ref=no_ref,
            ref_normalize_db=None if no_ref else -16.0,
            ref_ensure_max=True,
            num_candidates=1,
            decode_mode="sequential",
            seconds=None,
            duration_scale=duration_scale,
            max_ref_seconds=30.0,
            max_text_len=None,
            max_caption_len=None,
            num_steps=40,
            cfg_scale_text=3.0,
            cfg_scale_caption=3.0,
            cfg_scale_speaker=5.0,
            cfg_guidance_mode="independent",
            cfg_scale=None,
            cfg_min_t=0.5,
            cfg_max_t=1.0,
            truncation_factor=None,
            rescale_k=None,
            rescale_sigma=None,
            context_kv_cache=True,
            speaker_kv_scale=None,
            speaker_kv_min_t=None,
            speaker_kv_max_layers=None,
            speaker_uncond_mode="mask",
            seed=None,
            t_schedule_mode="linear",
            sway_coeff=-1.0,
            trim_tail=True,
            tail_window_size=20,
            tail_std_threshold=0.05,
            tail_mean_threshold=0.1,
            lora_adapter=None,
        )

    def synthesize(self, text: str, voice_ref_path: Path, speed: float) -> bytes:
        # speed 引数は OpenAI 互換 / API 拡張性のためにシグネチャに残してあるが、
        # 速度補正は Web Audio 側 (playbackRate) で一律に行う設計に揃えるため、
        # 合成側 duration_scale は 1.0 固定。voicevox 経路 (voicevox_core も speed は未渡し、
        # フロントの playbackRate で補正) との挙動対称性を保つ。
        _ = speed
        runtime = self._load_synth()
        request = self._make_request(
            text=text,
            caption=None,
            ref_wav=str(voice_ref_path),
            no_ref=False,
            duration_scale=1.0,
        )
        result = runtime.synthesize(request, log_fn=None)
        return _audio_to_wav_bytes(result.audio, int(result.sample_rate))

    def generate_voice_ref(self, caption: str, out_path: Path) -> None:
        runtime = self._load_voice_design()
        request = self._make_request(
            text=self.VOICE_REF_READING_TEXT,
            caption=caption,
            ref_wav=None,
            no_ref=True,
            duration_scale=1.0,
        )
        result = runtime.synthesize(request, log_fn=None)

        # upstream は save_wav ヘルパを提供しているのでそれを使う。soundfile への
        # 直接書き込みでも可だが、サンプルレート整数化など細かい挙動を任せる。
        from irodori_tts.inference_runtime import save_wav  # type: ignore

        out_path.parent.mkdir(parents=True, exist_ok=True)
        save_wav(str(out_path), result.audio, int(result.sample_rate))


def _audio_to_wav_bytes(audio, sample_rate: int) -> bytes:
    """torch.Tensor / numpy array → 16-bit PCM mono wav バイト列。

    upstream `save_wav` はファイル出力専用。HTTP body 用にバイト列が欲しいので
    soundfile (BytesIO) で同等のフォーマットに書き出す。

    InferenceRuntime.synthesize は `torch.Tensor` (shape `(channels, samples)`) を返すので、
    numpy 変換と (samples, channels) への transpose を行ってから書き込む (soundfile は
    `(samples,)` か `(samples, channels)` を受ける。 `(channels, samples)` だと 'Format
    not recognised' で失敗する)。
    """
    import soundfile as sf  # type: ignore

    try:
        import torch  # type: ignore

        if isinstance(audio, torch.Tensor):
            audio = audio.detach().cpu().numpy()
    except ImportError:
        pass

    # shape 整形: (1, N) → (N,) mono / (channels, N) → (N, channels)
    if hasattr(audio, "ndim") and audio.ndim == 2:
        if audio.shape[0] == 1:
            audio = audio[0]
        elif audio.shape[0] < audio.shape[1]:
            # 一般的に samples > channels なので、(channels, samples) と推定して転置
            audio = audio.T

    buf = io.BytesIO()
    sf.write(buf, audio, sample_rate, format="WAV", subtype="PCM_16")
    return buf.getvalue()


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
            # 実モデルモードで GPU が無いと InferenceRuntime のロード/合成は実用にならない。
            # 503 を返して Rust 側 (`IrodoriClient::health_ping`) に「異常」と認識させ、
            # `spawn_irodori_health_watcher` の 3 連続失敗カウンタに乗せる (architecture §8.6)。
            if gpu_name is None:
                return JSONResponse(
                    {"status": "no_gpu", "gpu": None, "mock": False},
                    status_code=503,
                )
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
    parser.add_argument(
        "--ready-file",
        type=Path,
        default=None,
        help="起動完了時に port/pid を書き出す JSON。--download-only モードでは未使用",
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0, help="0 で動的割当")
    parser.add_argument("--mock", action="store_true", help="実モデルを使わず正弦波 wav を返す")
    parser.add_argument(
        "--no-download",
        action="store_true",
        help="起動時の HF モデル DL を skip する (デバッグ / 事前 DL 済 用)",
    )
    parser.add_argument(
        "--download-only",
        action="store_true",
        help="HF モデルを DL したら即終了 (download_irodori_assets ステップ 6 用)。"
        " uvicorn は立てない。",
    )
    parser.add_argument("--log-level", default="warning")
    args = parser.parse_args(argv)

    logging.basicConfig(level=args.log_level.upper())
    asset_dir: Path = args.asset_dir
    asset_dir.mkdir(parents=True, exist_ok=True)

    # --download-only モード: HF モデルだけ DL して即終了。ready.json も書かない。
    # Rust 側 (irodori_download::install_irodori_models) が wait() で待つ。
    if args.download_only:
        try:
            download_models(asset_dir)
        except Exception as exc:
            sys.stderr.write(f"[hf-download] モデル DL 失敗: {exc}\n")
            return 1
        sys.stderr.write("[hf-download] モデル DL 完了\n")
        return 0

    port = args.port if args.port and args.port > 0 else pick_free_port(args.host)
    LOG.info("sidecar binding to %s:%d (mock=%s)", args.host, port, args.mock)

    backend: Optional[RealModelBackend] = None
    if not args.mock:
        # 通常の sidecar 起動経路では HF DL は走らせない (Rust 側で別ステップとして
        # 走らせる: irodori_download::install_irodori_models)。--no-download が無く
        # かつモデル不在の場合は RealModelBackend の synth が FileNotFoundError を投げて
        # 500 を返し、Rust 側で voicevox にフォールバックされる。
        if not args.no_download:
            try:
                download_models(asset_dir)
            except Exception as exc:
                sys.stderr.write(f"[hf-download] モデル DL 失敗: {exc}\n")
                return 1
        try:
            backend = RealModelBackend(asset_dir)
        except Exception as exc:
            sys.stderr.write(f"sidecar.py: backend 初期化失敗: {exc}\n")
            return 1

    if args.ready_file is None:
        sys.stderr.write("sidecar.py: --ready-file が必要です (--download-only を除く)\n")
        return 1

    app = build_app(asset_dir=asset_dir, mock=args.mock, backend=backend)

    # ready.json は **uvicorn が実際に listen を開始してから** 書く。
    # uvicorn.run を呼ぶ前や lifespan startup イベントで書くと、その時点ではまだ socket が
    # bind+listen されておらず、ugg が ready.json を検出して POST した瞬間に接続できず
    # reqwest が "error sending request" で失敗するレースになる。実機では実モデル初回発話が
    # これで irodori 失敗 → voicevox フォールバック → onnxruntime クラッシュに連鎖していた。
    # uvicorn.Server.started は listen 開始後に True になるので、別スレッドでそれを待って書く。
    config = uvicorn.Config(
        app,
        host=args.host,
        port=port,
        log_level=args.log_level,
        access_log=False,
    )
    server = uvicorn.Server(config)

    def _write_ready_when_listening() -> None:
        import time

        for _ in range(600):  # 最大 30 秒
            if server.started:
                try:
                    write_ready_file(args.ready_file, port)
                except OSError as exc:
                    sys.stderr.write(f"sidecar.py: ready file 書き出し失敗: {exc}\n")
                return
            time.sleep(0.05)
        sys.stderr.write("sidecar.py: uvicorn の listen 開始待ちでタイムアウト\n")

    import threading

    watcher = threading.Thread(target=_write_ready_when_listening, daemon=True)
    watcher.start()

    try:
        server.run()
    except SystemExit:
        raise
    except Exception as exc:  # pragma: no cover
        sys.stderr.write(f"sidecar.py: uvicorn 異常終了: {exc}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
