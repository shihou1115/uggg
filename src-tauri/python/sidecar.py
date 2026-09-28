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
        --download-only             HF モデルを取得したら終了（初回導入・更新）
        --synth-once --voice-ref W  1 回だけ合成して結果を報告し終了（更新の確認。v0.5.6）

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
import contextlib
import io
import json
import logging
import math
import os
import socket
import struct
import sys
import time
import wave
from pathlib import Path
from typing import Optional

# 切り替える前の stderr の文字コード（`log_stdio_encoding` が起動時に 1 行残す）。
ORIGINAL_STDERR_ENCODING = getattr(sys.stderr, "encoding", None)


def use_utf8_stdio() -> None:
    """stdout / stderr を UTF-8 にする (spec §6.0 v0.5.6 項目 2)。

    パイプへ書くとき、CPython は UTF-8 モードでなければ ANSI コードページ (日本語 Windows では
    cp932) で書く。同梱の Python は `._pth` で isolated なので、環境変数 (PYTHONIOENCODING /
    PYTHONUTF8) では変えられない。2026-09-19 に ugg が起動したサイドカーの中で
    `stderr.encoding=cp932` を観測し、Windows のエラー文が ugg.log で化けていた原因と確定した
    (Rust 側は UTF-8 で読む)。`-X utf8` は `open()` の既定の文字コードまで変えてモデル側のコードに
    影響しうるので使わず、stdio だけを切り替える。
    """
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="backslashreplace")
        except Exception:
            pass  # 切り替えられなくても、Rust 側が Shift_JIS で読み直す


# 依存の import より前に切り替える（依存が無いときのエラー文も日本語なので）。
use_utf8_stdio()

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
# 既定のモデル repo（**正本は Rust 側の `irodori_download::current_models()`**）。
#
# v0.5.5 項目 3: モデル ID をここに固定したままにすると、`sidecar.py` は
# **毎起動で無条件に上書きコピーされる**のに重みは初回 DL でしか取らないため、
# ID を変えた瞬間「コードだけ新しくなって重みが無い」状態になる。
# Rust から `--model-*` で渡させ、ここの値は**渡されなかったとき用の保険**に留める。
#
# v0.5.6 項目 3a: Rust が渡す値は経路で違う。**取得（--download-only）はいまのビルドの値、
# 起動（読み先）は導入記録から決めた値**で、更新が成功したときだけ記録がビルドに追いつく。
# v0.5.7 項目 2: ここの既定値は Rust の `MODEL_PINS` と同じにする（契約テストが見張る）。v0.5.6 は
# 「重みがある旧モデル（v3）に据え置く」としたが、乗り換えが成功すると旧モデルは消す（項目 10）ので、
# 重みがあるのは新しいほうになる。Rust は常に 3 つとも渡すので、既定値が使われるのは渡し忘れた経路だけ。
MODEL_REPO_SYNTH = "Aratako/Irodori-TTS-v4.1-Small"
# 参照音声の生成も v4.1 で行う（v0.5.7 項目 3。v2-VoiceDesign の取得 2 GB と別モデルの読み込みをやめる）。
MODEL_REPO_VOICE_DESIGN = "Aratako/Irodori-TTS-v4.1-Small"
MODEL_REPO_CODEC = "Aratako/Semantic-DACVAE-Japanese-32dim"
# 取得する revision。`main` は「そのとき最新」なので、pip の `refs/heads/main` と
# 同じく**上げても届かない / 黙って変わる**。Rust 側が固定値を渡せるようにしておく。
# コーデックも固定する（v0.5.7 項目 2）。`main` の間は repo ID で渡すので、上流が共有 HF キャッシュへ
# 取りに行き、新規の人は最初の合成で約 0.43 GB を黙って取っていた（`_codec_location`）。
MODEL_REVISION_SYNTH = "2b28324dc263ed5e6638b3cf3dd94c82ead07b4b"
MODEL_REVISION_VOICE_DESIGN = "2b28324dc263ed5e6638b3cf3dd94c82ead07b4b"
MODEL_REVISION_CODEC = "47376ee24834d7a05a48ebabfe3cde29b3c5e214"

# **モデルごとの値** (v0.5.7 項目 2・4、spec §6.0)。精度とサンプラーは、読み込むモデルで決める。
# ここに無いモデル（v3・v2-VoiceDesign）は v0.5.6 の値のまま — アプリを v0.5.7 に上げて Irodori の
# 更新をまだしていない間・更新に失敗して戻った間・VRAM 不足で保留した間は v3 で喋るので、そのとき
# 音と速さを変えない（v3 の bf16 はユーザーが聴いていない。v3 の sway 8 は 2026-09-14 に許容）。
# v4.1 の bf16 は 2026-09-26 にユーザーが聴いて許容（参照音声の生成では機械音声のクセが出るが許容範囲内）。
V4_MODELS = frozenset({"Aratako/Irodori-TTS-v4.1-Small"})
CODEC_PRECISION = "fp32"


def model_precision(repo: str) -> str:
    """合成モデルの精度。v4 系は bf16（VRAM が v3 より少ない。spec §6.0 の spike）、ほかは fp32。"""
    return "bf16" if repo in V4_MODELS else "fp32"


# 合成のサンプラー (spec §6.0 v0.5.6 項目 1)。
# v3 の通常合成は linear 40 → sway 8。参照音声の事前変換と合わせて約 3.6〜4.1 倍速（2026-09-14 の実測。
# 音はユーザーが自分の参照音声で聴いて許容と裁定）。`sway_coeff` は計測と同じ -1.0 のまま。
SYNTH_NUM_STEPS = 8
SYNTH_T_SCHEDULE = "sway"
# v4 系の既定（v0.5.7 項目 4 の設定の既定値と同じ 16）。ステップ数ごとのサンプラーは測った組のまま
# （8 は sway、16・40 は linear。spec §6.0 の spike）。
V4_DEFAULT_STEPS = 16
V4_STEP_SCHEDULES = {8: "sway", 16: "linear", 40: "linear"}


# 合成モデル・参照音声の生成モデルのリポジトリから取るもの（v0.5.7 項目 2）。Rust の
# `MODEL_FILES`（揃っているかの判定）と対。トークナイザを持たないリポジトリでは 2 つ目は何も当たらない。
WEIGHT_FILE_PATTERNS = ("model.safetensors", "tokenizer/*")


def synth_sampler(repo: str, steps: Optional[int] = None) -> tuple[int, str]:
    """通常合成のステップ数とサンプラー (v0.5.7 項目 4)。

    v4 系は設定のステップ数（`steps`。選択肢に無い・渡されないときは既定の 16）、**ほかのモデル（v3）は
    `steps` を無視して v0.5.6 の sway 8**（2026-09-26 ユーザー裁定。v3 で喋るのは更新前・失敗して戻った後・
    保留中で、そのとき音と速さを変えない）。
    """
    if repo in V4_MODELS:
        chosen = steps if steps in V4_STEP_SCHEDULES else V4_DEFAULT_STEPS
        return chosen, V4_STEP_SCHEDULES[chosen]
    return SYNTH_NUM_STEPS, SYNTH_T_SCHEDULE
# 参照音声の生成（VoiceDesign・no_ref）は据え置く。sway 8 を測ったのは参照音声つきの合成だけで、
# 生成は一度きりなので速さより品質が効く。用途 2 つの値を分けるだけで、設定の仕組みは作らない。
VOICE_DESIGN_NUM_STEPS = 40
VOICE_DESIGN_T_SCHEDULE = "linear"

# 参照音声の前処理。事前変換の結果の値を決めるので、変換結果のファイル名にも入れる。
REF_NORMALIZE_DB = -16.0
REF_ENSURE_MAX = True
MAX_REF_SECONDS = 30.0
REF_LATENT_SUFFIX = ".latent.pt"


def _apply_model_args(args) -> None:
    """Rust から渡されたモデルの正本を反映する (v0.5.5 項目 3)。

    渡されなかったものは既定値のまま（古い Rust と組み合わせても動く）。
    """
    global MODEL_REPO_SYNTH, MODEL_REPO_VOICE_DESIGN, MODEL_REPO_CODEC
    global MODEL_REVISION_SYNTH, MODEL_REVISION_VOICE_DESIGN, MODEL_REVISION_CODEC
    MODEL_REPO_SYNTH = args.model_synth or MODEL_REPO_SYNTH
    MODEL_REVISION_SYNTH = args.model_synth_revision or MODEL_REVISION_SYNTH
    MODEL_REPO_VOICE_DESIGN = args.model_voice_design or MODEL_REPO_VOICE_DESIGN
    MODEL_REVISION_VOICE_DESIGN = (
        args.model_voice_design_revision or MODEL_REVISION_VOICE_DESIGN
    )
    MODEL_REPO_CODEC = args.model_codec or MODEL_REPO_CODEC
    MODEL_REVISION_CODEC = args.model_codec_revision or MODEL_REVISION_CODEC


def model_dir_name(repo: str, revision: str) -> str:
    """モデルの置き場所。**revision を含める。**

    含めないと revision を上げたとき**同じパスへ上書き**になり、失敗しても戻れない
    （v0.5.3 項目 2 / v0.5.4 項目 3 の「旧版を消さない」と同じ規律）。
    """
    safe = repo.replace("/", "__")
    return f"{safe}@{revision}" if revision and revision != "main" else safe


def ref_latent_path(ref_wav: Path, model_key: str, precision: str) -> Path:
    """参照音声の事前変換の結果の置き場所 (spec §6.0 v0.5.6 項目 1)。**参照 wav の隣**に置く。

    変換結果の値は、変換を行うコーデック・形を整える合成モデル・それぞれの精度・参照の前処理で
    変わるので、全部をファイル名に入れる（`model_key` と `precision` は呼び出し側が両モデル分を
    つないで渡す。どれかが変われば別のファイルになり、古い結果を読まない）。ただし revision が
    `main` の間は、上流が中身を差し替えても名前は変わらない（固定の revision へ上げるまでの限界）。
    参照音声を消す・作り直すときは、Rust 側の `voice_ref::delete_file` が参照 wav と同じ
    ディレクトリの `<参照 wav の stem>.*.latent.pt`（書きかけの `.tmp` を含む）を一緒に消す。
    更新で旧モデルを片付けるとき（v0.5.7 項目 10）は `irodori_download::clean_up_old_models` が、
    いまの `<合成>+<コーデック>` を含まない変換結果を消す —
    **置き場所・名前の形・`.tmp` の付け方を変えるなら向こうも直す**（契約テストが見張る）。
    """
    prep = f"n{REF_NORMALIZE_DB:g}_e{int(REF_ENSURE_MAX)}_s{MAX_REF_SECONDS:g}"
    return ref_wav.with_name(
        f"{ref_wav.stem}.{model_key}.{precision}.{prep}{REF_LATENT_SUFFIX}"
    )



# --- リクエスト型 ----------------------------------------------------------

class SpeechRequest(BaseModel):
    """OpenAI `POST /v1/audio/speech` 互換 (architecture §8.5)。"""

    model: str = Field(..., description="モデル名 (mock では未使用)")
    input: str = Field(..., description="合成するテキスト (preprocess 済み想定)")
    voice: str = Field(..., description="参照音声 ID または絶対パス")
    response_format: str = Field("wav", description="現状 wav のみサポート")
    speed: float = Field(1.0, ge=0.25, le=4.0)
    caption: Optional[str] = Field(None, description="台本の声質・演技指示 (実モデルのみ有効)")
    # v0.5.7 項目 4: 設定のステップ数。使うかどうかは読み込んでいるモデルで決める（`synth_sampler`）。
    num_steps: Optional[int] = Field(None, description="通常合成のステップ数 (v4 系のみ有効)")


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

def _show_download_progress() -> None:
    """取得の進捗を、端末でなくても出させる（v0.5.6 項目 2）。

    huggingface_hub の進捗バー（tqdm）は、端末でないと出ない（`disable=None` の既定）。ugg は
    パイプで読むので、数 GB の取得中も画面が止まって見えた。hub は環境変数 `TQDM_POSITION=-1`
    のときだけ強制で出すが、それは tqdm の表示位置まで変え、カーソル移動の制御文字が混ざる。
    そこで判定の関数だけを「自動なら出す」に差し替える（明示の無効はそのまま）。上流の変更で
    見つからなければ、今までどおり出ないだけ（取得そのものは止めない）。
    """
    try:
        import importlib

        hub_tqdm = importlib.import_module("huggingface_hub.utils.tqdm")
        decide = hub_tqdm.is_tqdm_disabled

        def _shown_even_without_a_terminal(log_level):  # type: ignore[no-untyped-def]
            disabled = decide(log_level)
            return False if disabled is None else disabled

        hub_tqdm.is_tqdm_disabled = _shown_even_without_a_terminal
    except Exception as exc:
        _diag(f"[hf-download] 取得の進捗は出せません（取得は続けます）: {type(exc).__name__}")


def download_models(asset_dir: Path) -> None:
    """Aratako/Irodori-TTS 系モデルを `asset_dir/model/<repo>` に取得する。

    既に揃っていれば何もしない。stderr に進捗を出力し、Rust 側 (sidecar の child stderr)
    が `[hf-download] ...` 行を pick して `irodori-download` イベントへ転送する。
    """
    try:
        from huggingface_hub import snapshot_download  # type: ignore
    except ImportError:
        sys.stderr.write(
            "[hf-download] huggingface_hub が見つかりません。Irodori 資産 DL を実行してください\n"
        )
        raise
    _show_download_progress()
    target_root = asset_dir / "model"
    target_root.mkdir(parents=True, exist_ok=True)

    # `local_dir_use_symlinks` は渡さない（v0.5.6 項目 3c）。huggingface_hub 0.23 以降は非推奨で無視され
    # （警告の行を出すだけ）、1.x では引数ごと消えて TypeError になる。v0.5.7 には、新しい sidecar.py が
    # 更新前の依存のまま動く期間と、その逆の期間があるので、どちらの版でも通る形にしておく。
    # 合成 / VoiceDesign 本体は upstream infer.py と同じく `model.safetensors` でよい
    # (config 情報は safetensors のメタデータに埋め込まれている)。**v4 系はトークナイザも要る**
    # （v0.5.7 項目 2）。上流はチェックポイントの隣の `tokenizer/` があればローカルで読み、無ければ
    # 合成のときに `sbintuitions/modernbert-ja-310m` を黙って取りに行く。`tokenizer/` を持たない
    # リポジトリ（v3・v2-VoiceDesign）では何も増えない。
    weight_repos = [
        (MODEL_REPO_SYNTH, MODEL_REVISION_SYNTH),
        (MODEL_REPO_VOICE_DESIGN, MODEL_REVISION_VOICE_DESIGN),
    ]
    seen = set()
    for repo, revision in weight_repos:
        # 合成と参照音声の生成が同じモデルなら 1 回だけ確かめる（v0.5.7 項目 3）。
        if (repo, revision) in seen:
            continue
        seen.add((repo, revision))
        local_dir = target_root / model_dir_name(repo, revision)
        # **自前の「存在してサイズ > 0」判定をやめた** (v0.5.5 項目 3)。
        # 途中で切れた DL はサイズ > 0 のまま残るので、それでは完了と区別できない
        # （v0.5.2 の 0 バイト残骸と同型）。hub は etag を照合して
        # **一致していれば落とさない**ので、整合性の判断はそちらに委ねる。
        # 既に正しく入っている環境では通信はほぼ発生せず、再取得も起きない。
        sys.stderr.write(f"[hf-download] {repo}@{revision} を確認中…\n")
        local_dir.mkdir(parents=True, exist_ok=True)
        snapshot_download(
            repo_id=repo,
            revision=revision,
            local_dir=str(local_dir),
            allow_patterns=list(WEIGHT_FILE_PATTERNS),
        )
        sys.stderr.write(f"[hf-download] {repo} ダウンロード完了\n")

    # コーデック (DACVAE)。revision を固定している間は、ここに取った `weights.pth` をパスで読む
    # （`_codec_location`）。`main` の間は repo ID で渡し、上流が共有 HF キャッシュから読む。
    codec_dir = target_root / model_dir_name(MODEL_REPO_CODEC, MODEL_REVISION_CODEC)
    # コーデックも同じ理由で `snapshot_download` に判断を委ねる。
    sys.stderr.write(f"[hf-download] {MODEL_REPO_CODEC} を確認中…\n")
    snapshot_download(
        repo_id=MODEL_REPO_CODEC,
        revision=MODEL_REVISION_CODEC,
        local_dir=str(codec_dir),
    )
    sys.stderr.write(f"[hf-download] {MODEL_REPO_CODEC} ダウンロード完了\n")


# 透かし（SilentCipher）の重み（v0.5.7 項目 5）。上流の `irodori_tts/watermark.py` は、合成のランタイムを作るときに
# `silentcipher.get_model()` を呼び、重みが手元に無ければ `snapshot_download(repo_id="sony/silentcipher")` で
# **黙って取りに行く**（約 68 MB。共有 HF キャッシュへ。revision の指定なし）。更新の最初の段で同じ呼び方で先に取って、
# 合成のときに取りに行かせない（途中で全戻しして pydub だけ残ったときも — spec §6.0 の反証 3）。
WATERMARK_REPO = "sony/silentcipher"


def fetch_watermark_weights() -> None:
    """透かしの重みを、上流が読みに行く場所（共有 HF キャッシュ）へ先に取る。"""
    from huggingface_hub import snapshot_download  # type: ignore

    _show_download_progress()
    sys.stderr.write(f"[hf-download] 透かしの重み（{WATERMARK_REPO}）を確認中…\n")
    # 上流と**同じ呼び方**にする（repo だけ。revision も置き場所も指定しない）。違う形で取ると、
    # 上流が見る場所に入らず、合成のときにまた取りに行く。
    snapshot_download(repo_id=WATERMARK_REPO)
    sys.stderr.write(f"[hf-download] 透かしの重み（{WATERMARK_REPO}）ダウンロード完了\n")


# 上流が読む透かしの重み（`silentcipher.get_model(model_type="44.1k")` が snapshot の中のこの 4 つを読む）。
WATERMARK_WEIGHT_DIR = "44_1_khz/73999_iteration"
WATERMARK_WEIGHT_FILES = ("hparams.yaml", "enc_c.ckpt", "dec_c.ckpt", "dec_m_0.ckpt")


def hf_hub_cache_dir() -> Path:
    """共有 HF キャッシュの場所。huggingface_hub の `constants` と同じ規則で、**hub を import せずに**決める
    （hub は import した時点で `HF_HUB_OFFLINE` を読むので、決める前に import できない）。"""
    for name in ("HF_HUB_CACHE", "HUGGINGFACE_HUB_CACHE"):
        if os.environ.get(name):
            return Path(os.path.expanduser(os.environ[name]))
    home = os.environ.get("HF_HOME") or os.path.join(
        os.environ.get("XDG_CACHE_HOME") or os.path.join("~", ".cache"), "huggingface"
    )
    return Path(os.path.expanduser(home)) / "hub"


def watermark_weights_present(cache_dir: Path) -> bool:
    """透かしの重みが共有 HF キャッシュに揃っているか（`refs/main` が指す snapshot に 4 つとも、空でなく）。

    hub は取得し終えたファイルだけを snapshot に置くので、途中で切れた取得はここに現れない。
    """
    repo = cache_dir / ("models--" + WATERMARK_REPO.replace("/", "--"))
    try:
        sha = (repo / "refs" / "main").read_text(encoding="utf-8").strip()
    except OSError:
        return False
    if not sha:
        return False
    weights = repo / "snapshots" / sha / WATERMARK_WEIGHT_DIR
    try:
        return all((weights / name).stat().st_size > 0 for name in WATERMARK_WEIGHT_FILES)
    except OSError:
        return False


def stay_off_the_hub() -> str:
    """合成・参照音声の生成をするプロセスを、Hub へ問い合わせない形にする。説明の 1 行を返す（v0.5.8 項目 2）。

    上流は合成のランタイムを作るたびに、透かしの重みを `snapshot_download(repo_id=...)` で解決する（重みが手元に
    あっても `main` の版を Hub に聞く）。**誰も操作していないのに、サイドカーが起きるたびに外へ出る通信**で、
    通信が詰まる環境では読み込みが 9 → 30 秒になっていた（2026-09-28 の実測）。`HF_HUB_OFFLINE` は
    **上書きする**（`setdefault` にしない）。hub を import する前に呼ぶこと。

    **透かしの重みが揃っていないときは立てない**（spec §6.0 の反証 R1）。先取りは失敗しても導入・更新を止めない
    設計で、失敗の文言は「合成のときにもう一度取りに行きます」。立てると、その回復が消えて透かしが二度と効かない。
    """
    if watermark_weights_present(hf_hub_cache_dir()):
        os.environ["HF_HUB_OFFLINE"] = "1"
        return "Hub へは問い合わせません"
    return "透かしの重みが手元に揃っていないので、Hub への問い合わせを許します（合成のときに上流が取り直します）"


def watermark_state(runtime) -> tuple[bool, str]:
    """透かしが効いているかと、その説明（v0.5.7 項目 5）。効いていないことが見えなくならないように。"""
    ready = bool(getattr(getattr(runtime, "watermarker", None), "ready", False))
    if ready:
        return True, "効いています"
    try:
        import silentcipher  # type: ignore  # noqa: F401
    except Exception as exc:  # pydub が無い等（v0.5.6 まではこれで一度も効いていなかった）
        return False, f"効いていません（{type(exc).__name__}: {exc}）"
    return False, "効いていません（重みを読み込めませんでした）"


def avoid_cudnn_attention_plans() -> str:
    """上流の注意機構から cuDNN を外し、その説明を返す（v0.5.7 の実機検証 E-11 の 2a）。

    上流 `89f9d8fb` の `irodori_tts/attention.py` は、マスク付きの SDPA を
    `sdpa_kernel([CUDNN_ATTENTION, EFFICIENT_ATTENTION, MATH], set_priority=True)` で呼び、cuDNN を最優先に
    固定している（全体の `torch.backends.cuda.enable_cudnn_sdp(False)` では外せない）。cuDNN は**初めての入力の形
    ごとに実行計画を組み立てる**ので、長さが毎回違う ugg の台詞では 1 文ごとに約 700 ms 上乗せされていた
    （RTX 5080・16 ステップで 1115〜1290 ms。同じ文の 2 回目は 443〜481 ms）。EFFICIENT を先頭にすると初回も
    444〜491 ms で、定常の速さは変わらない（どちらも近似の無い計算）。上流にこの指定が無い版では何もしない。
    """
    try:
        import irodori_tts.attention as attention  # type: ignore
        from torch.nn.attention import SDPBackend  # type: ignore
    except Exception:
        return "上流の既定のまま（irodori_tts.attention が無い版）"
    if not hasattr(attention, "_SDPA_PRIORITY"):
        return "上流の既定のまま（優先順の指定が無い版）"
    attention._SDPA_PRIORITY = [SDPBackend.EFFICIENT_ATTENTION, SDPBackend.MATH]
    return "cuDNN を使わない（EFFICIENT → MATH）"


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
        # 参照音声の事前変換に一度失敗したら、このプロセスの間は参照 wav のまま合成する
        # （毎回失敗して遅くなるのを避ける。サイドカーは使わなければ 5 分で止まるので、次の起動でまた試す）。
        self._latent_disabled = False

    @staticmethod
    def _resolve_device() -> str:
        try:
            import torch  # type: ignore

            return "cuda" if torch.cuda.is_available() else "cpu"
        except Exception:
            return "cpu"

    def _checkpoint_path(self, repo: str, revision: str) -> Path:
        # revision に既定値を持たせない（2026-09-14 監査の掃討で外した）。既定値があると、
        # 呼び出し側が渡し忘れても黙って `main` の置き場所を読み、直した穴と同じ形に戻る。
        return self.asset_dir / "model" / model_dir_name(repo, revision) / "model.safetensors"

    def _codec_location(self) -> str:
        """コーデックの読み先。**`main` 以外の revision は、取得したその置き場所から読む。**

        `main` のときは従来どおり repo ID を渡す（ランタイムが HF キャッシュから読む。
        既存環境の挙動を変えない）。固定 revision を repo ID のまま渡すと、ランタイムは
        revision を知らずに `main` を読む。置き場所に無ければ読み込みで失敗させる
        （黙って別の版を読むより、理由がログに残るほうがよい）。
        """
        if MODEL_REVISION_CODEC == "main":
            return MODEL_REPO_CODEC
        return str(
            self.asset_dir
            / "model"
            / model_dir_name(MODEL_REPO_CODEC, MODEL_REVISION_CODEC)
            / "weights.pth"
        )

    def _build_runtime(self, repo: str, revision: str):
        """upstream infer.py の InferenceRuntime.from_key(RuntimeKey(...)) と同じ構成。

        **取得した revision の置き場所から読む**（2026-09-14 監査で発覚）。以前は
        `_checkpoint_path(repo)` で常に `main` の置き場所を読んでいた。Rust 側で revision を
        固定値へ上げると、既存環境は「更新済み」と記録したまま古い重みを読み続け、新規環境は
        FileNotFoundError で無言のフォールバックになる（取得側だけ直して読み込み側を残した形）。
        """
        from irodori_tts.inference_runtime import (  # type: ignore
            InferenceRuntime,
            RuntimeKey,
        )

        ckpt = self._checkpoint_path(repo, revision)
        if not ckpt.is_file():
            raise FileNotFoundError(
                f"model.safetensors が見つかりません: {ckpt}. download_models を先に実行してください"
            )
        device = self._resolve_device()
        _diag(f"[irodori] 注意機構: {avoid_cudnn_attention_plans()}")
        runtime = InferenceRuntime.from_key(
            RuntimeKey(
                checkpoint=str(ckpt),
                model_device=device,
                codec_repo=self._codec_location(),
                model_precision=model_precision(repo),
                codec_device=device,
                codec_precision=CODEC_PRECISION,
                codec_deterministic_encode=True,
                codec_deterministic_decode=True,
                compile_model=False,
                compile_dynamic=False,
            )
        )
        _, said = watermark_state(runtime)
        _diag(f"[irodori] 透かし: {said}")
        return runtime

    def _load_synth(self):
        if self._synth_runtime is None:
            self._synth_runtime = self._build_runtime(MODEL_REPO_SYNTH, MODEL_REVISION_SYNTH)
        return self._synth_runtime

    def watermark_ready(self) -> Optional[bool]:
        """読み込み済みの合成のランタイムで透かしが効いているか（まだ読み込んでいなければ None）。"""
        if self._synth_runtime is None:
            return None
        return watermark_state(self._synth_runtime)[0]

    def _load_voice_design(self):
        # **合成と同じモデルなら、読み込み済みの合成のランタイムを使い回す**（v0.5.7 項目 3）。上流の
        # `InferenceRuntime.from_key` はキャッシュしないので、別々に作ると同じモデルを 2 つ読み込み、
        # VRAM を 2 倍使う。モデルが違うのは、記録が旧モデル（v3 と v2-VoiceDesign）を指している間だけ。
        if (MODEL_REPO_VOICE_DESIGN, MODEL_REVISION_VOICE_DESIGN) == (MODEL_REPO_SYNTH, MODEL_REVISION_SYNTH):
            if self._synth_runtime is None:
                _diag("[irodori] 参照音声の生成は合成と同じモデルで行います（読み込みは 1 回）")
            return self._load_synth()
        if self._voice_design_runtime is None:
            self._voice_design_runtime = self._build_runtime(
                MODEL_REPO_VOICE_DESIGN, MODEL_REVISION_VOICE_DESIGN
            )
        return self._voice_design_runtime

    @staticmethod
    def _make_request(
        *,
        text: str,
        caption: Optional[str],
        ref_wav: Optional[str],
        ref_latent: Optional[str],
        no_ref: bool,
        duration_scale: float,
        num_steps: int,
        t_schedule_mode: str,
        seed: Optional[int] = None,
    ):
        """upstream infer.py のデフォルト引数群を写し取った SamplingRequest を組み立てる。

        ステップ数とサンプラーは用途（通常合成 / 参照音声の生成）ごとに呼び出し側が渡す
        （spec §6.0 v0.5.6 項目 1）。`ref_wav` と `ref_latent` は上流が同時指定を拒むので片方だけ。
        """
        from irodori_tts.inference_runtime import SamplingRequest  # type: ignore

        return SamplingRequest(
            text=text,
            caption=caption,
            ref_wav=ref_wav,
            ref_latent=ref_latent,
            ref_embed=None,
            no_ref=no_ref,
            ref_normalize_db=None if no_ref else REF_NORMALIZE_DB,
            ref_ensure_max=REF_ENSURE_MAX,
            num_candidates=1,
            decode_mode="sequential",
            seconds=None,
            duration_scale=duration_scale,
            max_ref_seconds=MAX_REF_SECONDS,
            max_text_len=None,
            max_caption_len=None,
            num_steps=num_steps,
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
            # 通常は固定しない。固定するのは受け入れ条件の確かめ（`acceptance_check`）だけ（spec §6.0 v0.5.7）
            seed=seed,
            t_schedule_mode=t_schedule_mode,
            sway_coeff=-1.0,
            trim_tail=True,
            tail_window_size=20,
            tail_std_threshold=0.05,
            tail_mean_threshold=0.1,
            lora_adapter=None,
        )

    def _synth_request(
        self,
        text: str,
        caption: Optional[str],
        ref_wav: Optional[str],
        ref_latent: Optional[str],
        steps: Optional[int] = None,
        seed: Optional[int] = None,
    ):
        """通常合成（参照音声つき）のリクエスト。ステップ数とサンプラーは読み込むモデルで決める。"""
        steps, schedule = synth_sampler(MODEL_REPO_SYNTH, steps)
        return self._make_request(
            text=text,
            caption=caption,
            ref_wav=ref_wav,
            ref_latent=ref_latent,
            no_ref=False,
            duration_scale=1.0,
            num_steps=steps,
            t_schedule_mode=schedule,
            seed=seed,
        )

    def _reference_latent(self, runtime, voice_ref_path: Path) -> tuple[Optional[str], bool]:
        """参照音声の事前変換の結果（ファイルのパス）と、今回作ったかどうかを返す
        (spec §6.0 v0.5.6 項目 1)。

        作れなければ None を返し、呼び出し側は今までどおり参照 wav を渡す（遅くなるだけで喋れる）。
        変換結果を作る公開 API は上流に無いので、ランタイム自身が合成のたびに使っている
        非公開のメソッド `_load_reference_latent` を 1 回だけ呼ぶ。2026-09-14 の計測と同じ方法で、
        値はランタイムの内部と同じになる（音はビット同一）。渡す側は公開 API（`ref_latent` は
        ファイルのパス）なので、結果はファイルに置く。
        """
        if self._latent_disabled:
            return None, False
        if not voice_ref_path.is_file():
            # 参照 wav そのものが無いのは事前変換の問題ではない。止めずに、今までどおり合成側の
            # エラーにする（ここで止めると、参照を作り直した後もこのプロセスの間は遅いままになる）。
            return None, False
        # 値を作るのはコーデック、形を整えるのは合成モデル。両方をキーに入れる。
        path = ref_latent_path(
            voice_ref_path,
            model_dir_name(MODEL_REPO_SYNTH, MODEL_REVISION_SYNTH)
            + "+"
            + model_dir_name(MODEL_REPO_CODEC, MODEL_REVISION_CODEC),
            f"{model_precision(MODEL_REPO_SYNTH)}-{CODEC_PRECISION}",
        )
        try:
            # 参照 wav のほうが新しければ作り直す（同じ名前のまま中身が変わった場合の保険）。
            if path.is_file() and path.stat().st_mtime >= voice_ref_path.stat().st_mtime:
                return str(path), False
            import torch  # type: ignore

            # テキストは変換に使われない。発話の本文を渡さない（失敗の理由がログに残るため）。
            request = self._synth_request(
                self.VOICE_REF_READING_TEXT, None, str(voice_ref_path), None
            )
            with torch.inference_mode():
                latent, _mask = runtime._load_reference_latent(
                    req=request, batch_size=1, messages=[]
                )
            tmp = path.with_name(path.name + ".tmp")
            torch.save(latent[0].detach().cpu().clone(), tmp)
            os.replace(tmp, path)
            return str(path), True
        except Exception as exc:
            if _is_out_of_memory(exc):
                # 事前変換のせいではない。止めずに、次の合成でまた作る
                _diag("[irodori] GPU のメモリが足りず参照音声を事前変換できないので、参照 wav で合成します")
                return None, False
            self._latent_disabled = True
            _diag(
                "[irodori] 参照音声の事前変換ができないので、このサイドカーが止まるまで参照 wav のまま"
                f"合成します: {type(exc).__name__}: {exc}"
            )
            return None, False

    def synthesize(
        self,
        text: str,
        voice_ref_path: Path,
        speed: float,
        caption: Optional[str] = None,
        steps: Optional[int] = None,
    ) -> bytes:
        # speed 引数は OpenAI 互換 / API 拡張性のためにシグネチャに残してあるが、
        # 速度補正は Web Audio 側 (playbackRate) で一律に行う設計に揃えるため、
        # 合成側 duration_scale は 1.0 固定。voicevox 経路 (voicevox_core も speed は未渡し、
        # フロントの playbackRate で補正) との挙動対称性を保つ。
        _ = speed
        # **参照音声のファイルが無いことを「モデルが無い」と取り違えない**（v0.5.7 リリース前監査）。参照 wav のまま
        # 上流へ渡すと、読み込みの `FileNotFoundError` が `synth_failure_kind` で `model_missing` になり、キャラが
        # 「更新する」で直ると事実でない案内をする。先に確かめ、`FileNotFoundError` でない例外で返す（種類は other）。
        if not voice_ref_path.is_file():
            raise RuntimeError("参照音声のファイルが見つかりません。設定の「音声」で参照音声を作り直してください")
        runtime = self._load_synth()
        started = time.perf_counter()
        latent, created = self._reference_latent(runtime, voice_ref_path)
        if latent is None:
            result = runtime.synthesize(
                self._synth_request(text, caption, str(voice_ref_path), None, steps), log_fn=None
            )
        else:
            try:
                result = runtime.synthesize(
                    self._synth_request(text, caption, None, latent, steps), log_fn=None
                )
            except Exception as exc:
                if _is_out_of_memory(exc):
                    raise  # 変換結果のせいではない。参照 wav でやり直しても同じく足りない
                _diag(
                    "[irodori] 参照音声の変換結果を使った合成に失敗したので、参照 wav でやり直します: "
                    f"{type(exc).__name__}"
                )
                # 参照 wav でも失敗したら、例外はそのまま上へ返る（本文などの問題で、変換結果は消さない）。
                result = runtime.synthesize(
                    self._synth_request(text, caption, str(voice_ref_path), None, steps), log_fn=None
                )
                # 参照 wav なら合成できた＝変換結果の側が合わない（壊れている等）。消して、このサイドカーが
                # 止まるまで使わない（作り直しても同じなら、毎回「失敗してやり直し」になって遅くなる）。
                try:
                    Path(latent).unlink()
                except OSError:
                    pass
                self._latent_disabled = True
                latent, created = None, False
        # 所要時間を 1 行残す（spec §6.0 v0.5.6 項目 1 の確かめ方）。本文と caption は残さない。
        # 変換結果を作った回はその時間も入るので、そうと分かるように書く。
        reference = "wav" if latent is None else ("latent（今回作成）" if created else "latent")
        # v0.5.7 項目 2: どのモデルと精度で喋ったかも残す（v3 で喋っている間と乗り換え後を見分ける）。
        used_steps, schedule = synth_sampler(MODEL_REPO_SYNTH, steps)
        _diag(
            f"[irodori] 合成 {(time.perf_counter() - started) * 1000:.0f} ms"
            f"（{MODEL_REPO_SYNTH.rsplit('/', 1)[-1]}・{model_precision(MODEL_REPO_SYNTH)}・"
            f"{used_steps} ステップ・{schedule}・参照 {reference}）"
        )
        return _audio_to_wav_bytes(result.audio, int(result.sample_rate))

    def generate_voice_ref(self, caption: str, out_path: Path) -> None:
        runtime = self._load_voice_design()
        request = self._make_request(
            text=self.VOICE_REF_READING_TEXT,
            caption=caption,
            ref_wav=None,
            ref_latent=None,
            no_ref=True,
            duration_scale=1.0,
            num_steps=VOICE_DESIGN_NUM_STEPS,
            t_schedule_mode=VOICE_DESIGN_T_SCHEDULE,
        )
        result = runtime.synthesize(request, log_fn=None)

        # upstream は save_wav ヘルパを提供しているのでそれを使う。soundfile への
        # 直接書き込みでも可だが、サンプルレート整数化など細かい挙動を任せる。
        from irodori_tts.inference_runtime import save_wav  # type: ignore

        out_path.parent.mkdir(parents=True, exist_ok=True)
        save_wav(str(out_path), result.audio, int(result.sample_rate))


def _diag(line: str) -> None:
    """診断の 1 行を stderr へ書く。書けなくても本来の処理を止めない
    （成功した合成を、ログが書けないせいで 500 にしない）。"""
    try:
        sys.stderr.write(line + "\n")
    except Exception:
        pass


def _is_out_of_memory(exc: BaseException) -> bool:
    """GPU のメモリ不足か（torch が無い・古い環境でも落ちない）。"""
    try:
        import torch  # type: ignore

        oom = getattr(getattr(torch, "cuda", None), "OutOfMemoryError", None)
        return oom is not None and isinstance(exc, oom)
    except Exception:
        return False


def synth_failure_kind(exc: BaseException) -> str:
    """合成の失敗の種類（v0.5.7 項目 7。Rust の `SynthFailure` と同じ組）。

    Rust はこれでキャラの説明を言い分ける（以前は理由によらず「GPU 環境が整っていない」と言った）。
    モデルの重みが無いときは `_build_runtime` が `FileNotFoundError` を投げる。**Hub へ問い合わせずに読むと
    （v0.5.8 項目 2）、キャッシュに無いものは別の例外で届く** — transformers は `OSError`（「couldn't find them in
    the cached files」）、hub は `LocalEntryNotFoundError`（`FileNotFoundError` の派生）や `OfflineModeIsEnabled`。
    どれも「揃っていない」（設定の「更新する」で直る）なので、原因を連ねた先まで見て同じ組にする。
    """
    if _is_out_of_memory(exc):
        return "oom"
    if _is_missing_locally(exc):
        return "model_missing"
    return "other"


def _is_missing_locally(exc: BaseException) -> bool:
    """手元に無いものを読もうとして失敗したか（例外の原因を連ねた先まで見る）。"""
    seen = set()
    cur: Optional[BaseException] = exc
    while cur is not None and id(cur) not in seen:
        seen.add(id(cur))
        if isinstance(cur, FileNotFoundError):
            return True
        if type(cur).__name__ in ("LocalEntryNotFoundError", "OfflineModeIsEnabled"):
            return True
        if isinstance(cur, OSError) and "cached files" in str(cur):
            return True
        cur = cur.__cause__ or cur.__context__
    return False


# --- 更新の成否を確かめる一発合成 (spec §6.0 v0.5.6 項目 3b) -----------------

# 報告の目印。Rust 側（irodori_download）と同じ文字列にする（契約テストが見張る）。
SYNTH_ONCE_START_MARKER = "UGG_SYNTH_ONCE_START "
SYNTH_ONCE_MARKER = "UGG_SYNTH_ONCE "
# 終了コード（Rust 側と揃える）。
SYNTH_ONCE_OK = 0
SYNTH_ONCE_FAILED = 1
SYNTH_ONCE_OOM = 2
SYNTH_ONCE_NO_GPU = 3
# 読み上げる固定文。**発話の本文は渡さない**（失敗の理由がログに残るため）。
SYNTH_ONCE_TEXT = "こんにちは。更新の確認です。"
# 更新の確認で参照音声の生成を試すときのキャプション（v0.5.7 項目 6）。作った声はゲートの作業場所に置き、終わったら消す。
SYNTH_ONCE_CAPTION = "落ち着いた声で、はっきりと話す。"


def _report(marker: str, payload: dict) -> None:
    sys.stdout.write(marker + json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def _exit_now(code: int) -> None:
    """結果を書き終えたプロセスを、**後片付けを通さずに**終わらせる（v0.5.8 項目 3）。

    CUDA を使ったあとの通常の終了（インタプリタの後片付け・ネイティブのスレッドの終了）で落ちると、終了コードが 0 で
    なくなり、確かめた成功が失敗として扱われる（`classify_gate` は結果の行が `ok` でも終了コードを見る）。結果は
    もう書いてあるので、stdout / stderr を flush してから `os._exit` で抜ける。後片付けで固まって締め切りまで
    待たされることも避けられる。**結果の行を書く前に落ちたものは、いまどおり失敗として届く。**
    """
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except Exception:
            pass
    os._exit(code)


def synth_once(asset_dir: Path, voice_ref: Optional[Path], gate_dir: Optional[Path] = None) -> int:
    """更新の成否を確かめるため、1 回だけ合成する (spec §6.0 v0.5.6 項目 3b)。

    **v0.5.7 項目 6: 参照音声の生成も 1 回試す**（`gate_dir` を渡されたとき）。乗り換えでは生成も新しいモデルに
    まとめ、成功したら旧モデルを消すので、生成を試さずに通すと、生成しか手段の無い人（参照音声が無い人）が
    戻る先を失う。生成した声は `gate_dir` に置き、`voice_ref` が無ければその声で合成する（参照音声が無い人でも
    合成と生成の両方を確かめられる）。

    更新中は HTTP の経路が錠で塞がっているので、更新処理の子プロセスとして走らせる。結果は stdout の
    目印付きの 1 行と終了コードで返す。**VRAM 不足では例外にならずプロセスごと落ちることがある**
    （v0.5.4 の実機で実績）ので、モデルを読む前に空き VRAM を 1 行出しておく（落ちたときの手がかり）。
    参照音声は Rust が一時フォルダへ写したものを渡す（事前変換の結果が参照 wav の隣に作られるため、
    ユーザーの refs を汚さない）。
    """
    try:
        import torch  # type: ignore
    except Exception as exc:
        _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "other", "error": f"{type(exc).__name__}: {exc}"})
        return SYNTH_ONCE_FAILED
    cuda = bool(torch.cuda.is_available())
    start = {"cuda": cuda, "torch_cuda": getattr(torch.version, "cuda", None)}
    if cuda:
        try:
            free, total = torch.cuda.mem_get_info()
            start["vram_free_mb"] = int(free // (1024 * 1024))
            start["vram_total_mb"] = int(total // (1024 * 1024))
        except Exception:
            pass
    _report(SYNTH_ONCE_START_MARKER, start)
    if not cuda:
        _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "no_gpu"})
        return SYNTH_ONCE_NO_GPU
    started = time.perf_counter()
    backend = RealModelBackend(asset_dir)
    voice_design = None
    if gate_dir is not None:
        generated = gate_dir / "generated.wav"
        vd_started = time.perf_counter()
        try:
            backend.generate_voice_ref(SYNTH_ONCE_CAPTION, generated)
            if not generated.is_file() or generated.stat().st_size == 0:
                raise RuntimeError("生成した参照音声が空でした")
        except Exception as exc:
            if _is_out_of_memory(exc):
                _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "oom", "stage": "voice_design", "error": type(exc).__name__})
                return SYNTH_ONCE_OOM
            _report(SYNTH_ONCE_MARKER, {
                "ok": False,
                "kind": "other",
                "stage": "voice_design",
                "error": f"参照音声の生成: {type(exc).__name__}: {exc}",
            })
            return SYNTH_ONCE_FAILED
        voice_design = {"ok": True, "ms": int((time.perf_counter() - vd_started) * 1000)}
        if voice_ref is None:
            voice_ref = generated
    if voice_ref is None:
        _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "other", "error": "合成に使う参照音声がありません"})
        return SYNTH_ONCE_FAILED
    try:
        wav = backend.synthesize(SYNTH_ONCE_TEXT, voice_ref, 1.0, None)
    except Exception as exc:
        if _is_out_of_memory(exc):
            _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "oom", "stage": "synth", "error": type(exc).__name__})
            return SYNTH_ONCE_OOM
        _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "other", "stage": "synth", "error": f"{type(exc).__name__}: {exc}"})
        return SYNTH_ONCE_FAILED
    if not wav:
        _report(SYNTH_ONCE_MARKER, {"ok": False, "kind": "other", "error": "合成結果が空でした"})
        return SYNTH_ONCE_FAILED
    ms = int((time.perf_counter() - started) * 1000)
    # v0.5.7 項目 5: 透かしが効いているかも返す（効いていなくても合成はできるので合否には使わない）。
    _report(SYNTH_ONCE_MARKER, {
        "ok": True,
        "ms": ms,
        "bytes": len(wav),
        "watermark": backend.watermark_ready(),
        "voice_design": voice_design,
    })
    return SYNTH_ONCE_OK


# --- 受け入れ条件の確かめ（試験の経路だけ。spec §6.0 v0.5.7） ----------------------------

ACCEPTANCE_SEED = 1234
ACCEPTANCE_TEXT = "今日はいい天気ですね。少し散歩に行きませんか。"
# 台本の caption の例（script-reader-spec の S4 と同じ）
ACCEPTANCE_CAPTION = "驚いて大声で"
# 透かしの中身（"IRDTS"）。上流の `irodori_tts/watermark.py` が埋める値
WATERMARK_PAYLOAD = [73, 82, 68, 84, 83]
# 上流の EMOJI_ANNOTATIONS.md の 45 種。**Rust の `preprocess::IRODORI_EMOJIS` と同じ並び**（契約テストが突き合わせる）
ACCEPTANCE_EMOJIS = (
    "👂",
    "😮\u200d💨",
    "⏸\ufe0f",
    "🤭",
    "🥵",
    "📢",
    "😏",
    "🥺",
    "🌬\ufe0f",
    "😮",
    "👅",
    "💋",
    "🫶",
    "😭",
    "😱",
    "😪",
    "😴",
    "⏩",
    "📞",
    "🐢",
    "🥤",
    "🤧",
    "😒",
    "😰",
    "😆",
    "💥",
    "😠",
    "😲",
    "🥱",
    "😖",
    "😟",
    "🫣",
    "🙄",
    "😊",
    "😎",
    "👌",
    "🙏",
    "🥴",
    "🎵",
    "🤐",
    "😌",
    "🤔",
    "💪",
    "👃",
    "📖",
)


def _mono(audio):
    """合成結果（torch.Tensor の (channels, samples) か numpy）を 1 次元の numpy にする。"""
    try:
        import torch  # type: ignore

        if isinstance(audio, torch.Tensor):
            audio = audio.detach().float().cpu().numpy()
    except ImportError:
        pass
    if getattr(audio, "ndim", 1) == 2:
        audio = audio[0]
    return audio


def acceptance_check(asset_dir: Path, voice_ref: Path, out_dir: Path) -> int:
    """v0.5.7 の受け入れ条件のうち、手元の実物で確かめる 3 つ (spec §6.0)。**試験の経路だけに置く**
    （seed を固定する手段を HTTP の要求やユーザー向けの設定に足さない）。

    1. caption: seed を固定し、同じ文・同じ参照音声で caption なしとありを合成して差が出る。対照として、
       同じ seed・同じ caption の 2 回がビット同一（seed を固定しない比較は、何を足しても必ず差が出る）
    2. 絵文字 45 種: 合成モデルのトークナイザで未知語にならない（音が変わるかは条件にしない）
    3. 透かし: 出力から IRDTS を読み取れる。対照として、透かしを外した出力からは読み取れない

    結果は `out_dir/acceptance.json` と聴き比べ用の wav に残す。参照音声は `out_dir` へ写してから使う
    （ユーザーの refs には何も書かない）。終了コードは全部通れば 0、どれか落ちれば 1、確かめられなければ 2。
    """
    import hashlib
    import shutil

    out_dir.mkdir(parents=True, exist_ok=True)
    report: dict = {"model": f"{MODEL_REPO_SYNTH}@{MODEL_REVISION_SYNTH}", "seed": ACCEPTANCE_SEED}

    def finish(code: int) -> int:
        report["passed"] = code == 0
        (out_dir / "acceptance.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return code

    try:
        import numpy as np  # type: ignore
        import soundfile as sf  # type: ignore
        import torch  # type: ignore
    except Exception as exc:
        report["error"] = f"{type(exc).__name__}: {exc}"
        return finish(2)
    if not torch.cuda.is_available():
        report["error"] = "GPU が見えません"
        return finish(2)
    if not voice_ref.is_file():
        report["error"] = f"参照音声がありません: {voice_ref}"
        return finish(2)
    ref = out_dir / "ref.wav"
    shutil.copyfile(voice_ref, ref)

    backend = RealModelBackend(asset_dir)
    runtime = backend._load_synth()

    def synth(name: str, caption: Optional[str]) -> dict:
        req = backend._synth_request(ACCEPTANCE_TEXT, caption, str(ref), None, None, seed=ACCEPTANCE_SEED)
        res = runtime.synthesize(req, log_fn=None)
        audio = np.ascontiguousarray(_mono(res.audio), dtype=np.float32)
        sr = int(res.sample_rate)
        sf.write(str(out_dir / f"{name}.wav"), audio, sr)
        return {"audio": audio, "sr": sr, "sha": hashlib.sha256(audio.tobytes()).hexdigest()}

    # 1. caption
    plain = synth("caption_none", None)
    with_caption = synth("caption_on_1", ACCEPTANCE_CAPTION)
    with_caption_again = synth("caption_on_2", ACCEPTANCE_CAPTION)
    report["caption"] = {
        "caption": ACCEPTANCE_CAPTION,
        "differs": plain["sha"] != with_caption["sha"],
        "control_identical": with_caption["sha"] == with_caption_again["sha"],
    }

    # 2. 絵文字
    tok = runtime.tokenizer.tokenizer
    unk = tok.unk_token_id
    unknown = [e for e in ACCEPTANCE_EMOJIS if unk is not None and unk in tok.encode(e, add_special_tokens=False)]
    report["emoji"] = {"count": len(ACCEPTANCE_EMOJIS), "unknown": unknown}

    # 3. 透かし（`plain` は透かしが入った出力。外した出力を同じ seed で作って比べる）
    watermarker = getattr(runtime, "watermarker", None)
    report["watermark"] = {"ready": bool(getattr(watermarker, "ready", False))}
    if watermarker is not None and getattr(watermarker, "model", None) is not None:
        saved = watermarker.model
        watermarker.model = None
        try:
            unmarked = synth("watermark_off", None)
        finally:
            watermarker.model = saved
        import librosa  # type: ignore
        import silentcipher  # type: ignore

        detector = silentcipher.get_model(model_type="44.1k", device="cuda")

        def read(item: dict) -> dict:
            y, sr = item["audio"], item["sr"]
            if sr != 44100:
                y = librosa.resample(y, orig_sr=sr, target_sr=44100)
                sr = 44100
            r = detector.decode_wav(y, sr, phase_shift_decoding=False)
            messages = [list(map(int, m)) for m in (r.get("messages") or [])]
            return {
                "status": bool(r.get("status")),
                "messages": messages,
                "confidences": [round(float(c), 3) for c in (r.get("confidences") or [])],
                "found": WATERMARK_PAYLOAD in messages,
            }

        report["watermark"]["on"] = read(plain)
        report["watermark"]["off"] = read(unmarked)

    ok = (
        report["caption"]["differs"]
        and report["caption"]["control_identical"]
        and not report["emoji"]["unknown"]
        and report["watermark"].get("on", {}).get("found", False)
        and not report["watermark"].get("off", {}).get("found", True)
    )
    return finish(0 if ok else 1)


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

def is_peer_reset_on_close(context: dict) -> bool:
    """ugg 側が接続を切ったあと、Windows の asyncio（Proactor）が接続を閉じる途中の `shutdown()` で出す
    `ConnectionResetError` か (spec §6.0 v0.5.7 項目 11)。

    合成は成功しているのに、`_ProactorBasePipeTransport._call_connection_lost` のトレースバックが 6 行ずつ
    ugg.log に残っていた（2026-09-24 の実機で合成 6 回のうち 2 回。test-plan E-10）。**この 1 つの形だけ**を
    落とす。同じ `ConnectionResetError` でも別の場所で出たもの、`_call_connection_lost` でも別の例外は残す。
    """
    if not isinstance(context.get("exception"), ConnectionResetError):
        return False
    callback = getattr(context.get("handle"), "_callback", None)
    return getattr(callback, "__name__", None) == "_call_connection_lost"


def quiet_peer_reset(loop: asyncio.AbstractEventLoop, context: dict) -> None:
    """asyncio の例外ハンドラ。接続切れの 1 つの形だけを落とし、ほかは既定のハンドラへ渡す（今までどおり残る）。"""
    if is_peer_reset_on_close(context):
        return
    loop.default_exception_handler(context)


@contextlib.asynccontextmanager
async def _install_quiet_peer_reset(_app):
    """uvicorn が作るイベントループに、上のハンドラを付ける（`on_event` は FastAPI で非推奨の警告を出すので使わない）。"""
    asyncio.get_running_loop().set_exception_handler(quiet_peer_reset)
    yield


def build_app(asset_dir: Path, mock: bool, backend: Optional[RealModelBackend]) -> FastAPI:
    app = FastAPI(
        title="ugg-irodori-sidecar",
        docs_url=None,
        redoc_url=None,
        lifespan=_install_quiet_peer_reset,
    )

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
                    caption=req.caption,
                    steps=req.num_steps,
                )
            except NotImplementedError as exc:
                raise HTTPException(501, str(exc))
            except Exception as exc:
                # v0.5.7 項目 7: 理由の種類を添える（`{"detail": {"kind": ..., "message": ...}}`）
                raise HTTPException(
                    500, {"kind": synth_failure_kind(exc), "message": f"Irodori 合成失敗: {exc}"}
                )
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


def log_stdio_encoding() -> None:
    """stderr の文字コードを起動時に 1 行残す (spec §6.0 v0.5.6 項目 2)。

    v0.5.5 の実環境では、サイドカーの stderr に cp932 の行が混ざっていた。2026-09-19 にこの行で
    ugg が起動したサイドカーの中を観測し、`stderr.encoding=cp932` だったので `use_utf8_stdio` で
    切り替えるようにした。切り替えが効いているかを確かめるため、切り替える前の文字コード
    (`original`) と切り替えた後の文字コードを並べて残す。
    `sample` は日本語の見本で、Rust 側の ugg.log で読めれば正しく届いている。`sample_hex` は
    Python が書くつもりのバイト列で、ASCII なのでどの文字コードで読んでも崩れない。
    """
    import locale

    sample = "既存の接続"
    try:
        enc = getattr(sys.stderr, "encoding", None)
        try:
            sample_hex = sample.encode(enc or "utf-8", errors="replace").hex()
        except LookupError:
            sample_hex = "?"
        sys.stderr.write(
            "[stdio] "
            f"stderr.encoding={enc} (original={ORIGINAL_STDERR_ENCODING}) "
            f"errors={getattr(sys.stderr, 'errors', None)} "
            f"locale={locale.getpreferredencoding(False)} "
            f"utf8_mode={sys.flags.utf8_mode} isolated={sys.flags.isolated} "
            f"no_site={sys.flags.no_site} isatty={sys.stderr.isatty()} "
            f"sample={sample} sample_hex={sample_hex}\n"
        )
        sys.stderr.flush()
    except Exception as exc:  # 診断の 1 行で起動を止めない
        try:
            sys.stderr.write(f"[stdio] 文字コードの確認に失敗: {exc!r}\n")
        except Exception:
            pass


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
    parser.add_argument(
        "--synth-once",
        action="store_true",
        help="1 回だけ合成して結果を報告し、即終了する（更新の成否の確認用。v0.5.6 項目 3b）。"
        " uvicorn は立てない。--voice-ref が必要",
    )
    parser.add_argument("--voice-ref", type=Path, default=None, help="--synth-once で使う参照 wav")
    parser.add_argument(
        "--gate-dir",
        type=Path,
        default=None,
        help="--synth-once で参照音声の生成も試す作業場所（v0.5.7 項目 6）。生成した声はここに置く",
    )
    parser.add_argument(
        "--fetch-watermark",
        action="store_true",
        help="透かしの重みを共有 HF キャッシュへ先に取って即終了する（v0.5.7 項目 5）。uvicorn は立てない",
    )
    parser.add_argument(
        "--acceptance",
        type=Path,
        default=None,
        help="受け入れ条件（caption・絵文字・透かし）を確かめて、結果と wav をこのフォルダに置いて終了する"
        "（試験の経路だけ。spec §6.0 v0.5.7）。--voice-ref が必要。uvicorn は立てない",
    )
    # **モデルの正本は Rust 側** (v0.5.5 項目 3)。渡されなければ上の既定値を使う。
    # ここをハードコードのままにすると、`sidecar.py` は毎起動で上書きされるのに
    # 重みは初回 DL でしか取らないため、ID を変えた瞬間に重みだけ無い状態になる。
    parser.add_argument("--model-synth", default=None)
    parser.add_argument("--model-synth-revision", default=None)
    parser.add_argument("--model-voice-design", default=None)
    parser.add_argument("--model-voice-design-revision", default=None)
    parser.add_argument("--model-codec", default=None)
    parser.add_argument("--model-codec-revision", default=None)
    parser.add_argument("--log-level", default="warning")
    args = parser.parse_args(argv)

    _apply_model_args(args)
    logging.basicConfig(level=args.log_level.upper())
    asset_dir: Path = args.asset_dir
    asset_dir.mkdir(parents=True, exist_ok=True)

    # --fetch-watermark モード（v0.5.7 項目 5）: 透かしの重みだけ取って即終了。更新の最初の段で呼ばれる。
    if args.fetch_watermark:
        try:
            fetch_watermark_weights()
        except Exception as exc:
            sys.stderr.write(f"[hf-download] 透かしの重みの取得に失敗: {type(exc).__name__}: {exc}\n")
            return 1
        return 0

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

    # ここから下は合成・参照音声の生成をするモード。**Hub へは問い合わせない**（v0.5.8 項目 2）。取得と更新の段
    # （上の 2 つ）だけが通信する。例外は `--no-download` 無しでサーバーとして起動したとき（手で起動したときだけ。
    # ugg は必ず `--no-download` を渡す）で、その中で取得が走るので立てない。hub を import する前に決める。
    serves_with_download = (
        args.acceptance is None and not args.synth_once and not args.mock and not args.no_download
    )
    if serves_with_download:
        _diag("[irodori] Hub: 取得を含む起動なので、問い合わせを許します")
    else:
        _diag(f"[irodori] Hub: {stay_off_the_hub()}")

    # --acceptance モード（v0.5.7 の受け入れ条件）: 試験の経路だけ。HTTP は立てない。
    if args.acceptance is not None:
        if args.voice_ref is None:
            sys.stderr.write("sidecar.py: --acceptance には --voice-ref が必要です\n")
            return 2
        # 結果（acceptance.json と標準出力）を書いたら後片付けを通さずに抜ける（v0.5.8 項目 3）
        _exit_now(acceptance_check(asset_dir, args.voice_ref, args.acceptance))

    # --synth-once モード（v0.5.6 項目 3b）: 1 回だけ合成して即終了。--download-only と同じく、
    # ポート確保と --ready-file の必須チェックより前に置く（HTTP は立てない）。モデルは取りに行かない
    # （無ければ合成が失敗する ＝ 更新で重みが揃わなかったことを捕まえるのがこのモードの役目）。
    if args.synth_once:
        if args.voice_ref is None and args.gate_dir is None:
            sys.stderr.write("sidecar.py: --synth-once には --voice-ref か --gate-dir が必要です\n")
            return SYNTH_ONCE_FAILED
        # 結果の行を書いたら後片付けを通さずに抜ける（v0.5.8 項目 3。後片付けで落ちて、確かめた成功を捨てない）
        _exit_now(synth_once(asset_dir, args.voice_ref, args.gate_dir))

    port = args.port if args.port and args.port > 0 else pick_free_port(args.host)
    LOG.info("sidecar binding to %s:%d (mock=%s)", args.host, port, args.mock)
    # --download-only の後に置く: そちらの出力は進捗としてユーザーの画面に流れるので、診断の行を混ぜない
    log_stdio_encoding()

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
