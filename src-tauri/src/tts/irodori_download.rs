//! Irodori-TTS の Python ランタイム + 共通依存の初回 DL (architecture §8.2-8.3, M4c Phase C/G)。
//!
//! 配置は `%APPDATA%\ugg\irodori\python\` 配下。
//! - `python.exe` / `python311.dll` 等: 公式 Embeddable Python (Windows x64) を ZIP で取得
//! - `Lib\site-packages\`: pip ブートストラップで作成、torch / fastapi / uvicorn / huggingface_hub
//!   + Phase G 追加で transformers / peft / accelerate / safetensors / librosa / numba / 等 +
//!   irodori-tts / dacvae / silentcipher を GitHub アーカイブから取得
//!
//! HF モデル本体 (Aratako/Irodori-TTS-*) の DL は `sidecar.py::download_models` が担う (Phase G)。
//! 本モジュールは「Python が起動して `from irodori_tts.inference_runtime import InferenceRuntime`
//! できる状態」までを担う。
//!
//! 設計判断:
//! - Python 3.11.x: torch CUDA wheel が最も安定して提供されている系列 (3.13 はまだ部分対応)
//! - torch >=2.10 / cu128: Irodori-TTS upstream pyproject の要求に合わせる (cu121 から更新)
//! - GitHub アーカイブ URL での pip install: embeddable Python に git CLI が無いため `git+` URL は
//!   使えない。代わりに `https://github.com/.../archive/<ref>.zip` でアーカイブを取得し pip に渡す
//! - `--no-deps`: irodori-tts pyproject の `dacvae` / `silentcipher` git+ 依存をスキップし
//!   別 step で明示的に install (順序: silentcipher → dacvae → irodori-tts)
//! - zip 展開は PowerShell の `Expand-Archive` 呼び出し: 追加 crate なし
//! - run_python は出力を**行ごとに**流す（v0.5.6 項目 2。以前は終わってから一括で読んでおり、数 GB の
//!   取得中は画面が 1 行のまま固まった）。無進捗が続けば止める。仕組みは `tts::child_process`

use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::tts::child_process::{self, Ended, Line, Stream};

use anyhow::{anyhow, Context, Result};

/// 採用 Python バージョン (CUDA 12.x torch wheel が安定して提供されている系列)。
pub const PYTHON_VERSION: &str = "3.11.9";
/// 公式 Embeddable Python (Windows x64) の DL URL。
const PYTHON_URL: &str = "https://www.python.org/ftp/python/3.11.9/python-3.11.9-embed-amd64.zip";
/// pip ブートストラップ用スクリプト。
const GET_PIP_URL: &str = "https://bootstrap.pypa.io/get-pip.py";
/// torch CUDA 12.8 wheel の追加 index URL。
/// Irodori-TTS upstream pyproject が `torch>=2.10.0` を要求するため Phase G で cu121→cu128 に更新。
const TORCH_CUDA_INDEX_URL: &str = "https://download.pytorch.org/whl/cu128";

/// Phase C で確実にインストールする共通依存。バージョン固定で再現性を担保。
/// torch 系は CUDA index 経由で別途インストールする (`install_torch_cuda`)。
///
/// `huggingface_hub` は **transformers 5 が連鎖して入れる版そのもの**に固定する（v0.5.7 項目 1）。
/// 以前は `==0.27.0` と書きながら、transformers の依存に押し上げられて実物は 0.36.2 だった
/// （要件と記録が食い違い、「版を固定して再現性を担保」が成り立っていなかった）。
/// 配布名の綴りは記録の名前になるので変えない（`huggingface-hub` と書くと別の名前として残る）。
const COMMON_REQUIREMENTS: &[&str] = &[
    "fastapi==0.115.6",
    "uvicorn[standard]==0.32.1",
    "huggingface_hub==1.33.0",
    "numpy<2",
    "soundfile==0.12.1",
];

/// torch / torchaudio バージョン (CUDA 12.8)。
/// Irodori-TTS upstream pyproject の `torch>=2.10.0,<2.11.0` レンジに合わせる。
const TORCH_PACKAGES: &[&str] = &["torch>=2.10.0,<2.11.0", "torchaudio>=2.10.0,<2.11.0"];

/// Irodori-TTS が要求する追加 pip パッケージ (Phase G)。
/// upstream pyproject の dependencies と Phase G で実 InferenceRuntime に必要な周辺ライブラリを
/// 過不足なく揃える (`dacvae` / `silentcipher` / `irodori-tts` 本体は GitHub アーカイブで別途)。
///
/// **transformers 5 系は spike で確かめた組み合わせに固定する**（v0.5.7 項目 1、spec §6.0）。
/// 上流は `transformers>=5.12.1,<6` と範囲で宣言しているが、範囲で入れると利用者の環境が
/// 確かめていない組み合わせになり、将来の 5.x が求める hub と `huggingface_hub` の固定が衝突して
/// 更新が失敗し続けうる。`tokenizers` は transformers の依存だが、同じ理由で版を明示する。
/// `pydub` は `silentcipher`（透かし）の import に要る（無いと一度も透かしが効かなかった。項目 5）。
const IRODORI_EXTRA_REQUIREMENTS: &[&str] = &[
    "torchcodec>=0.10.0,<0.11.0",
    "transformers==5.17.0",
    "tokenizers==0.23.2",
    "pydub==0.25.1",
    "accelerate>=1.0.0",
    "peft>=0.18.0",
    "safetensors>=0.7.0",
    "datasets>=3.0.0",
    "librosa",
    "numba>=0.57.0",
    "llvmlite>=0.40.0",
    "sentencepiece>=0.1.99,<0.2",
    "pyyaml>=6.0",
    "tqdm>=4.67.3",
    "einops",
    // dacvae の transitive 依存 (PyPI 名は descript-audiotools、import 名は audiotools)。
    // upstream Irodori-TTS pyproject では dacvae の git+ 依存だけ宣言されているが、それ自体は
    // descript-audiotools を要件として書いておらず、実合成 (DAC codec encode/decode) で
    // `No module named 'audiotools'` が発生する。明示的に追加。
    "descript-audiotools>=0.7.2",
];

/// silentcipher (Sesame AI Labs) の固定 commit zipball。upstream Irodori-TTS pyproject の
/// `silentcipher @ git+https://github.com/SesameAILabs/silentcipher.git@<hash>` と同じ hash。
const SILENTCIPHER_ZIPBALL: &str =
    "https://github.com/SesameAILabs/silentcipher/archive/d46d7d0893a583d8968ab3a6626e2289faec9152.zip";

/// dacvae (facebookresearch) の**固定 commit** zipball。upstream Irodori-TTS pyproject の
/// `dacvae = { git = "https://github.com/facebookresearch/dacvae" }` と同じリポジトリ。
/// pin: 2025-12-22 時点の main HEAD。
const DACVAE_ZIPBALL: &str =
    "https://github.com/facebookresearch/dacvae/archive/414c20785fc3a28373073ea8ef7a1316eeeaca6e.zip";

/// Irodori-TTS 本体 (Aratako) の**固定 commit** zipball。`infer.py` / `irodori_tts.inference_runtime`
/// を提供する。pin: 2026-09-12 時点の main HEAD（`89f9d8fb`、MeanFlow と v4-Large の対応。v0.5.7 項目 1）。
/// `8224daf` から `pyproject.toml` は変わっていない（どちらも `transformers>=5.12.1,<6` を宣言）。
/// 参照音声の事前変換に使う非公開の `_load_reference_latent` の形も同じ（spec §6.0 の spike）。
///
/// **3 資産すべてを commit 固定にする** (v0.4 負債返済 D4)。以前は本体と dacvae が
/// `refs/heads/main` 追随で、上流の破壊的変更がそのまま配布版の初回 DL を壊しうる状態だった
/// (silentcipher だけが pin 済みという非一貫)。更新するときは
/// **ここを手で上げて実機で DL・合成まで通す**こと。
const IRODORI_TTS_ZIPBALL: &str =
    "https://github.com/Aratako/Irodori-TTS/archive/89f9d8fbd4d51ea019867ee1197725ede1df13c5.zip";

/// Python 配置ディレクトリ (`%APPDATA%\ugg\irodori\python\`)。
/// Phase D 以降の `sidecar.py` 起動で使う。
#[allow(dead_code)]
pub fn python_dir() -> Result<PathBuf> {
    Ok(crate::tts::voice_ref::irodori_root()?.join("python"))
}

/// Python 実行ファイル (`python.exe`)。Phase D 以降のサイドカー起動で使う。
#[allow(dead_code)]
pub fn python_exe() -> Result<PathBuf> {
    Ok(python_dir()?.join("python.exe"))
}

/// Irodori 資産が「実モデル可」レベルまで揃っているか。
/// Phase C (python.exe + torch + fastapi + uvicorn + huggingface_hub) + Phase G (irodori_tts) を要求。
/// この判定が true のときのみ設定パネルの「実モデルを使う (β)」トグルが enable される。
/// 導入記録のファイル名。`%APPDATA%\ugg\irodori\installed.json`。
const STAMP_FILE: &str = "installed.json";
/// 導入記録のスキーマ版。形を変えたら上げる（読めない版は「記録なし」として扱う）。
const STAMP_SCHEMA: u32 = 1;
/// 解決済みバージョンを python から受け取るときの目印。
const VERSIONS_MARKER: &str = "UGG_RESOLVED_VERSIONS ";

/// 「何を入れたか」の記録 (v0.5.4 項目 1)。
///
/// **なぜ要るか**: `assets_ready` はパッケージの存在しか見ないため、pin を上げても
/// 既存環境には永久に届かない。届いていないことに気づく手段が、アプリ側に 1 つも無かった
/// （pip の `direct_url.json` を読まないと分からない）。
///
/// **`pins` と `resolved` を両方持つ理由**は役割が違うから:
/// - `pins` = このビルドが**要求した**もの。いまの定数と突き合わせて「入れ直しが要るか」を決める
/// - `resolved` = **実際に入った**もの。指定どおりに入るとは限らない
///   （`huggingface_hub==0.27.0` と書いてあるのに実機は 0.36.2 だった。transformers の
///   依存に押し上げられたため）。記録するのは指定値ではなく実測値でなければ意味がない
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstalledStamp {
    pub schema: u32,
    /// 導入が完了した unix 秒。
    pub installed_at: i64,
    /// このビルドが要求した固定 URL（名前 → URL）。
    pub pins: std::collections::BTreeMap<String, String>,
    /// 実際に入ったバージョン（配布名 → 版。取得できなければ欠落）。
    pub resolved: std::collections::BTreeMap<String, String>,
    /// このビルドが要求した**名前付き pip 要件**（配布名 → 要件文字列）。
    ///
    /// **`pins`（固定 URL）だけでは足りない** (v0.5.5 項目 3)。`transformers<5` を
    /// `>=5` に変えても `outdated` は空のままで、**更新ボタンすら出なかった**。
    /// v0.5.4 は「モデルが対象外」と書いたが、**対象外なのはモデルだけではなかった。**
    ///
    /// 判定は**要件文字列そのものの比較**で行う（`<5` を満たすかの版比較はしない）。
    /// 見たいのは「このビルドが要求するものが変わったか」なので、これで足りる。
    #[serde(default)]
    pub requirements: std::collections::BTreeMap<String, String>,
    /// このビルドが要求した **HF モデル**（名前 → `repo@revision`）。
    ///
    /// v0.5.4 の更新経路はモデルを見ていなかった。`sidecar.py` は毎起動で上書きされるのに
    /// 重みは初回 DL でしか取らないので、**ID を変えると静かに壊れる**。
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, String>,
}

/// いまのビルドが要求している固定 URL 一式。
///
/// **ここに挙げたものだけが「入れ直しが要るか」の判定材料になる。**
/// pin を増やしたらここにも足すこと（`pins_cover_every_pinned_url` が件数で見張る）。
pub fn current_pins() -> std::collections::BTreeMap<String, String> {
    [
        ("python", PYTHON_URL),
        ("silentcipher", SILENTCIPHER_ZIPBALL),
        ("dacvae", DACVAE_ZIPBALL),
        ("irodori_tts", IRODORI_TTS_ZIPBALL),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// いまのビルドが要求している**名前付き pip 要件**一式（配布名 → 要件文字列）。
///
/// `pip install <spec>` で名前指定して入れるものすべて。固定 URL（`current_pins`）とは
/// 入れ方が違うので分けて持つ。
pub fn current_requirements() -> std::collections::BTreeMap<String, String> {
    COMMON_REQUIREMENTS
        .iter()
        .chain(TORCH_PACKAGES.iter())
        .chain(IRODORI_EXTRA_REQUIREMENTS.iter())
        .map(|spec| (requirement_name(spec).to_string(), spec.to_string()))
        .collect()
}

/// HF モデルの正本 (v0.5.5 項目 3、spec §6.0)。
///
/// **`sidecar.py` のハードコードを正本にしない。** あのファイルは
/// `install_sidecar_script` が**毎起動で無条件に上書きコピー**するのに、重みは
/// 初回 DL でしか取らない。ID をあちらに置いたまま変えると、**コードだけ新しくなって
/// 重みが無い**状態になり、`decide_fallback` が全部フォールバックさせるので
/// **高品質モードが無言で VOICEVOX に落ちる**（「届かない」ではなく「静かに壊れる」）。
///
/// `revision` が `main` なのは**現状の追認**であって固定ではない。pip の
/// `refs/heads/main` と同じく「そのとき最新」なので、上げても届かないし黙って変わる。
/// **この仕組みが入ったことで、固定値へ変えれば既存環境へ届くようになる。**
///
/// **v0.5.7 項目 2**: 合成を v3 から `Aratako/Irodori-TTS-v4.1-Small`（bf16。精度は `sidecar.py` が
/// モデルで決める）へ。コーデックも revision を固定する（`main` の間は上流が共有 HF キャッシュから
/// 読み、新規の人は最初の合成で約 0.43 GB を黙って取っていた）。
const MODEL_PINS: &[(&str, &str, &str)] = &[
    (
        "model_synth",
        "Aratako/Irodori-TTS-v4.1-Small",
        "2b28324dc263ed5e6638b3cf3dd94c82ead07b4b",
    ),
    // **参照音声の生成も同じ v4.1 で行う**（v0.5.7 項目 3）。欄は消さずに中身を合成と同じ値にする —
    // 読み先の決定（`pick_models_to_read`）はビルドの名前しか回さないので、欄を消すと引数が渡らず
    // `sidecar.py` の既定値が使われる。同じ値なら取得は 1 回で、サイドカーは合成のランタイムを使い回す。
    (
        "model_voice_design",
        "Aratako/Irodori-TTS-v4.1-Small",
        "2b28324dc263ed5e6638b3cf3dd94c82ead07b4b",
    ),
    (
        "model_codec",
        "Aratako/Semantic-DACVAE-Japanese-32dim",
        "47376ee24834d7a05a48ebabfe3cde29b3c5e214",
    ),
];

/// いまのビルドのモデルが**揃っている**と言えるファイル（`model\<置き場所>\` からの相対パス。v0.5.7 項目 2）。
///
/// v4.1 はトークナイザをチェックポイントの隣に置く（`sidecar.py` の `WEIGHT_FILE_PATTERNS` で取る）。
/// 無ければ上流が合成のときに `sbintuitions/modernbert-ja-310m` を黙って取りに行く。**欠けていても
/// 「未導入」にはしない**（`assets_ready` の意味は変えない — 変えると、更新前の人の設定が消えて更新ボタンも
/// 隠れる。spec §6.0 の反証 1）。記録がいまのビルドのモデルを指しているのに欠けていれば、「更新が要る」に数える。
const MODEL_FILES: &[(&str, &[&str])] = &[
    (
        "model_synth",
        &["model.safetensors", "tokenizer/tokenizer.json", "tokenizer/tokenizer_config.json"],
    ),
    // 生成も同じ v4.1（caption のトークナイザも同じ `tokenizer/` を読む）
    (
        "model_voice_design",
        &["model.safetensors", "tokenizer/tokenizer.json", "tokenizer/tokenizer_config.json"],
    ),
    ("model_codec", &["weights.pth"]),
];

/// モデルの置き場所の名前。**`sidecar.py` の `model_dir_name` と同じ規則**（契約テストが見張る）:
/// `/` を `__` に、revision が `main` 以外なら `@revision` を付ける。
fn model_dir_name(repo: &str, revision: &str) -> String {
    let safe = repo.replace('/', "__");
    if revision.is_empty() || revision == "main" {
        safe
    } else {
        format!("{safe}@{revision}")
    }
}

/// いまのビルドのモデルのうち、記録はそれを指しているのにファイルが欠けているものの名前。
fn incomplete_models(
    asset_root: &Path,
    recorded_models: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let model_root = asset_root.join("model");
    MODEL_PINS
        .iter()
        .filter(|(name, repo, rev)| recorded_models.get(*name) == Some(&format!("{repo}@{rev}")))
        .filter(|(name, repo, rev)| {
            let dir = model_root.join(model_dir_name(repo, rev));
            let files = MODEL_FILES
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, files)| *files)
                .unwrap_or(&[]);
            files.iter().any(|f| !dir.join(f).is_file())
        })
        .map(|(name, _, _)| name.to_string())
        .collect()
}

/// いまのビルドが要求している HF モデル一式（名前 → `repo@revision`）。
pub fn current_models() -> std::collections::BTreeMap<String, String> {
    MODEL_PINS
        .iter()
        .map(|(name, repo, rev)| (name.to_string(), format!("{repo}@{rev}")))
        .collect()
}

/// **欄が空の記録が指す環境の中身** — v0.5.4 と v0.5.5 が入れていた要件（固定値）。
///
/// v0.5.4 が書いた記録には要件・モデルの欄が無い。記録そのものが無い環境（v0.5.4 より前の
/// 導入）も同じ。そうした環境に入っているのは**当時のビルドが入れた版**であって、
/// **いまのビルドが求める版ではない**。
///
/// **`current_requirements()` / `current_models()` で代用してはいけない**（2026-09-14 監査で発覚）。
/// 代用すると、要件を変えたビルドが「いまの値」を基準値として書き、差が出ずに**更新が
/// 永久に届かない**（v0.5.5 がまさに直した穴の再発）。v0.5.5 のうちは両者が一致するので鳴らない。
/// **要件やモデルを変えても、この定数は変えない。**
const V054_BASELINE_REQUIREMENTS: &[&str] = &[
    "fastapi==0.115.6",
    "uvicorn[standard]==0.32.1",
    "huggingface_hub==0.27.0",
    "numpy<2",
    "soundfile==0.12.1",
    "torch>=2.10.0,<2.11.0",
    "torchaudio>=2.10.0,<2.11.0",
    "torchcodec>=0.10.0,<0.11.0",
    "transformers<5",
    "accelerate>=1.0.0",
    "peft>=0.18.0",
    "safetensors>=0.7.0",
    "datasets>=3.0.0",
    "librosa",
    "numba>=0.57.0",
    "llvmlite>=0.40.0",
    "sentencepiece>=0.1.99,<0.2",
    "pyyaml>=6.0",
    "tqdm>=4.67.3",
    "einops",
    "descript-audiotools>=0.7.2",
];

/// [`V054_BASELINE_REQUIREMENTS`] と同じ理由で固定するモデル（名前 → `repo@revision`）。
const V054_BASELINE_MODELS: &[(&str, &str)] = &[
    ("model_synth", "Aratako/Irodori-TTS-500M-v3@main"),
    (
        "model_voice_design",
        "Aratako/Irodori-TTS-500M-v2-VoiceDesign@main",
    ),
    ("model_codec", "Aratako/Semantic-DACVAE-Japanese-32dim@main"),
];

fn v054_baseline_requirements() -> std::collections::BTreeMap<String, String> {
    V054_BASELINE_REQUIREMENTS
        .iter()
        .map(|spec| (requirement_name(spec).to_string(), spec.to_string()))
        .collect()
}

fn v054_baseline_models() -> std::collections::BTreeMap<String, String> {
    V054_BASELINE_MODELS
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// **取得しに行く先**（いまのビルドが求めるモデル）。`--download-only` で使う。
///
/// 読み先（サイドカーの起動で渡す値）とは**別**にする（v0.5.6 項目 3a）。以前は同じ値を両方へ渡していたが、
/// それだと定数を変えた版を入れた瞬間に、**重みが無いモデルを読みに行く**（更新を終えるまで高品質モードが死に、
/// キャラは「GPU 環境が整っていない」と事実でない説明をする）。**取得はいまのビルド、読みは記録**で、
/// **更新が成功したときだけ記録がいまのビルドに追いつく**。
pub fn model_args_for_fetch() -> Vec<String> {
    #[cfg(test)]
    if let Some(args) = FETCH_ARGS_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return args;
    }
    model_args_from(&current_models())
}

/// **テストのビルドだけにある差し込み口**（v0.5.7 の受け入れ条件 (b)、test-plan E-11 の A5）。取得先を差し替えて、
/// 実物の更新を「入れ替えたあと、モデルの取得で落ちる」経路へ通す。出荷物には入らない。
#[cfg(test)]
static FETCH_ARGS_FOR_TEST: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// **読みに行く先**（記録から決める。v0.5.6 項目 3a の決定表）。サイドカーの起動で使う。
/// 引数の並びと、**どこから決めたか**（ログに残す）を返す。
pub fn model_args_for_read(asset_root: &Path) -> (Vec<String>, &'static str) {
    let stamp = read_stamp(asset_root);
    (
        model_args_from(&models_to_read(stamp.as_ref())),
        where_models_come_from(stamp.as_ref()),
    )
}

/// 読み先の決定表（純関数）。**名前ごとに引く。**
///
/// | その名前の状態 | 読み先 |
/// |---|---|
/// | 記録の `models` に値がある | その値 |
/// | 記録が無い・その名前の欄が無い | `V054_BASELINE_MODELS`（欄が空の記録が指す環境の中身） |
/// | 基準値にもその名前が無い（v0.5.7 でモデルを増やしたとき） | いまのビルド |
///
/// **記録全体ではなく名前ごとに引く**のは、`models` が名前ごとに欠けうるため（`merged_models` は入れ直した分しか
/// 書かず、`baseline_filled` は欄が**丸ごと**空のときしか埋めない）。記録全体を単位にすると「1 本だけ欠けた記録」の
/// 読み先が未定義になり、渡さなかった分は `sidecar.py` の既定値が使われて記録と食い違う。
pub(crate) fn models_to_read(
    stamp: Option<&InstalledStamp>,
) -> std::collections::BTreeMap<String, String> {
    let recorded = stamp.map(|s| s.models.clone()).unwrap_or_default();
    pick_models_to_read(&recorded, &v054_baseline_models(), &current_models())
}

/// 決定表の中身（3 つの表から名前ごとに選ぶだけの純関数）。
///
/// **表を引数で受け取る**のは、v0.5.6 では基準値といまのビルドが**同じ値**で、本物の定数では
/// 「名前ごとに引く」と「記録全体で引く」の違いが出ないため（初めて違いが出るのは v0.5.7）。
/// 違う値で固定しておかないと、規則を壊してもテストが鳴らない。
fn pick_models_to_read(
    recorded: &std::collections::BTreeMap<String, String>,
    baseline: &std::collections::BTreeMap<String, String>,
    build: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    build
        .keys()
        .map(|name| {
            let value = recorded
                .get(name)
                .or_else(|| baseline.get(name))
                .or_else(|| build.get(name))
                .cloned()
                .unwrap_or_default();
            (name.clone(), value)
        })
        .collect()
}

/// 読み先を**どこから決めたか**（実機で追えるようにログへ出す。v0.5.6 項目 3a）。
pub(crate) fn where_models_come_from(stamp: Option<&InstalledStamp>) -> &'static str {
    match stamp {
        Some(s) if current_models().keys().all(|k| s.models.contains_key(k)) => "記録",
        Some(s) if !s.models.is_empty() => "記録と基準値",
        _ => "基準値",
    }
}

/// 名前 → `repo@revision` の表を、サイドカーへ渡す引数の並びにする。
fn model_args_from(models: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, value) in models {
        let flag = match name.as_str() {
            "model_synth" => "--model-synth",
            "model_voice_design" => "--model-voice-design",
            "model_codec" => "--model-codec",
            _ => continue,
        };
        // 記録は `repo@revision` の 1 つの文字列。`@` はモデル名に出てこないので最後の `@` で切る。
        let (repo, rev) = match value.rsplit_once('@') {
            Some((repo, rev)) if !repo.is_empty() && !rev.is_empty() => (repo, rev),
            _ => (value.as_str(), "main"),
        };
        out.push(flag.to_string());
        out.push(repo.to_string());
        out.push(format!("{flag}-revision"));
        out.push(rev.to_string());
    }
    out
}

fn stamp_path(asset_root: &Path) -> PathBuf {
    asset_root.join(STAMP_FILE)
}

/// 導入記録を読む。無い・壊れている・スキーマが違う場合は `None`
/// （= v0.5.4 より前に導入した環境。記録が無いこと自体が「古い」の証拠になる）。
pub fn read_stamp(asset_root: &Path) -> Option<InstalledStamp> {
    let text = std::fs::read_to_string(stamp_path(asset_root)).ok()?;
    let stamp: InstalledStamp = serde_json::from_str(&text).ok()?;
    (stamp.schema == STAMP_SCHEMA).then_some(stamp)
}

/// `pins` を明示して記録を書く。
///
/// **部分更新のあとは「入れ直した分だけ」を書き換える。** 全部を現在値にすると、
/// まだ古いままの依存（`ensure_python_embeddable` が skip する Python 本体など）まで
/// 「最新」と記録してしまい、記録そのものが嘘になる。
pub fn write_stamp_pins(
    asset_root: &Path,
    pins: std::collections::BTreeMap<String, String>,
    resolved: std::collections::BTreeMap<String, String>,
    requirements: std::collections::BTreeMap<String, String>,
    models: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let stamp = InstalledStamp {
        schema: STAMP_SCHEMA,
        installed_at: chrono::Utc::now().timestamp(),
        pins,
        resolved,
        requirements,
        models,
    };
    let json = serde_json::to_string_pretty(&stamp).context("導入記録の JSON 化")?;
    // **途中の状態を読ませない**（v0.5.6 項目 3a）。書き込みは truncate → 書き込みなので、その間に
    // 読むと壊れた JSON になり、`read_stamp` はそれを「記録なし」に畳む。読み先を記録から決めるように
    // なったので、そのときの読み先は基準値へ倒れる。`sidecar.py` の変換結果の書き出しと同じ形にする。
    let path = stamp_path(asset_root);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .with_context(|| format!("導入記録の書き出し: {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("導入記録の差し替え: {}", path.display()))?;
    Ok(())
}

/// 要件文字列から配布名だけを取り出す。
/// `"uvicorn[standard]==0.32.1"` → `"uvicorn"` / `"torch>=2.10.0,<2.11.0"` → `"torch"`。
fn requirement_name(spec: &str) -> &str {
    let end = spec
        .find(|c: char| matches!(c, '[' | '=' | '<' | '>' | '!' | '~' | ';' | ' '))
        .unwrap_or(spec.len());
    spec[..end].trim()
}

/// 記録対象の配布名（重複を除いた順序保持）。
/// pip で名前を指定して入れたもの全部 + GitHub アーカイブで入れた 3 本。
fn recorded_distributions() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let named = COMMON_REQUIREMENTS
        .iter()
        .chain(TORCH_PACKAGES.iter())
        .chain(IRODORI_EXTRA_REQUIREMENTS.iter())
        .map(|s| requirement_name(s).to_string());
    for name in named.chain(
        ["silentcipher", "dacvae", "irodori-tts"]
            .into_iter()
            .map(str::to_string),
    ) {
        if !out.iter().any(|n| n == &name) {
            out.push(name);
        }
    }
    out
}

/// `VERSIONS_MARKER` 付きの行から解決済みバージョンを取り出す。
fn parse_resolved_line(line: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let json = line.trim().strip_prefix(VERSIONS_MARKER)?;
    serde_json::from_str(json).ok()
}

/// 実際に入ったバージョンを python に聞く。
///
/// **失敗しても導入を失敗にしない。** 記録が取れないこと自体は動作に影響しないので、
/// 空の記録を返して続行する（`pins` だけでも「入れ直しが要るか」は判定できる）。
fn query_resolved_versions<F>(
    py_exe: &Path,
    mut on_line: F,
) -> std::collections::BTreeMap<String, String>
where
    F: FnMut(&str),
{
    let names = recorded_distributions();
    let script = format!(
        "import json,importlib.metadata as m
out={{}}
for n in {names:?}:
    try: out[n]=m.version(n)
    except Exception: pass
print({marker:?}+json.dumps(out,sort_keys=True))",
        names = names,
        marker = VERSIONS_MARKER,
    );
    let mut found = None;
    let res = run_python(py_exe, &["-c", &script], |line| {
        if let Some(map) = parse_resolved_line(line) {
            found = Some(map);
        } else {
            on_line(line);
        }
    });
    if let Err(err) = res {
        on_line(&format!("導入バージョンの記録に失敗しました (続行します): {err:#}"));
    }
    found.unwrap_or_default()
}

/// 導入記録を残す（`download_irodori_assets` の最後に呼ぶ、v0.5.4 項目 1）。
pub fn record_installed<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    // 初回導入は「入れ直せる 3 本」と、**名前付き要件とモデルの全部**を入れたことになる。
    // **`python` をここに含めない** — `ensure_python_embeddable` は `python.exe` があれば
    // skip するので、「入れた」と「入っている」は一致しない。実物に聞いて一致したときだけ記録する。
    //
    // **要件とモデルも記録する**（2026-09-14 監査で発覚）。以前は固定 URL の 3 本しか渡さず、
    // 要件・モデルの欄が空のまま書かれて、あとで基準値を書き足す処理に頼っていた。
    let installed: Vec<String> = current_pins()
        .keys()
        .filter(|k| updatable_pin(k).is_some())
        .cloned()
        .chain(current_requirements().into_keys())
        .chain(current_models().into_keys())
        .collect();
    record_after_install(asset_root, &installed, |l| on_line(l))?;
    on_line("導入内容を記録しました");
    Ok(())
}

/// 導入・更新のあとに記録を書く（両経路で共通）。
///
/// **`python` は実物が pin と一致したときだけ記録する。** 初回導入の経路もここを通す。
/// 通していなかったため、`ensure_python_embeddable` が skip した古い python を
/// 「最新」と記録し、以後 `should_ask_python` が実物に聞かなくなって
/// **永久に見えなくなる**穴が残っていた（更新経路だけ塞いで隣を残していた）。
fn record_after_install<F>(asset_root: &Path, installed: &[String], mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    let stamp = read_stamp(asset_root);
    let recorded = stamp.as_ref().map(|s| s.pins.clone()).unwrap_or_default();
    let stamp_reqs = stamp.as_ref().map(|s| s.requirements.clone());
    let stamp_models = stamp.as_ref().map(|s| s.models.clone());
    let mut resolved = stamp.map(|s| s.resolved).unwrap_or_default();
    // 記録の要点は「指定した版」ではなく「実際に入った版」。毎回取り直す。
    resolved.extend(query_resolved_versions(&py_exe, |l| on_line(l)));
    let py_version = installed_python_version(asset_root);
    let python_matches = py_version.as_deref() == pinned_python_version();
    if let Some(v) = py_version {
        resolved.insert("python".to_string(), v);
    }
    let recorded_reqs = stamp_reqs.unwrap_or_default();
    write_stamp_pins(
        asset_root,
        merged_pins(&recorded, installed, python_matches),
        resolved,
        merged_requirements(&recorded_reqs, installed),
        merged_models(&stamp_models.unwrap_or_default(), installed),
    )
}

/// 入れ直せた分だけを現在値へ反映した記録用の `models` を作る。
/// `merged_pins` / `merged_requirements` と同じ規律（開発方針 7 の「対になる関数」）。
fn merged_models(
    recorded: &std::collections::BTreeMap<String, String>,
    installed: &[String],
) -> std::collections::BTreeMap<String, String> {
    let current = current_models();
    let mut out = recorded.clone();
    for name in installed {
        if let Some(v) = current.get(name) {
            out.insert(name.clone(), v.clone());
        }
    }
    out
}

/// 入れ直せた分だけを現在値へ反映した記録用の `requirements` を作る。
///
/// `merged_pins` と同じ規律 — **入れ直していないものまで現在値にすると記録が嘘になる**。
fn merged_requirements(
    recorded: &std::collections::BTreeMap<String, String>,
    installed: &[String],
) -> std::collections::BTreeMap<String, String> {
    let current = current_requirements();
    let mut reqs = recorded.clone();
    for name in installed {
        if let Some(spec) = current.get(name) {
            reqs.insert(name.clone(), spec.clone());
        }
    }
    reqs
}

/// 導入状態 (v0.5.4 項目 2)。
///
/// **「使える」と「最新」は別の質問。** `present` が真なら実モデルは使える。
/// `up_to_date` が偽でも使えることに変わりはない（古いコードで古いモデルを動かしている
/// だけ）。ここを混ぜて `assets_ready` を偽にすると、フロントの
/// `canUseReal = gpuOk && assetsOk` が倒れ、**ユーザーの
/// `tts_irodori_use_real_model` が黙って false に書き換わって永続化される**。
/// 動いている環境を壊さないために、2 つの信号は分けたままにする。
#[derive(Debug, Clone, serde::Serialize)]
pub struct IrodoriStatus {
    /// パッケージが一式そろっているか（従来の `assets_ready` と同じ意味）。
    pub present: bool,
    /// **導入記録があるか。** v0.5.4 より前に導入した環境では無い
    /// （記録が無いこと自体が「いつの版か分からない」の証拠）。
    pub has_record: bool,
    /// いまのビルドが要求する pin と、記録された pin が一致するか。
    /// 記録が無ければ `false`（分からないものを「最新」とは言わない）。
    pub up_to_date: bool,
    /// 一致しなかった pin の名前。`up_to_date` が偽の理由を示す。
    pub outdated: Vec<String>,
    /// 記録されている「実際に入った版」。
    pub resolved: std::collections::BTreeMap<String, String>,
}

/// pin されている Python の版（`PYTHON_URL` の `python-3.11.9-embed-amd64.zip` → `3.11.9`）。
pub fn pinned_python_version() -> Option<&'static str> {
    PYTHON_URL
        .rsplit('/')
        .next()?
        .strip_prefix("python-")?
        .split('-')
        .next()
}

/// 入っている Python の版を**実物に聞く**（`python.exe --version` → `Python 3.11.9`）。
///
/// 記録を当てにしないのは、`ensure_python_embeddable` が `python.exe` があれば skip
/// するため、**「記録が無い」ことと「版が違う」ことが別物**だから。混同すると、
/// 直せもしない入れ直しを要求して更新経路そのものが止まる。
pub fn installed_python_version(asset_root: &Path) -> Option<String> {
    let py_exe = asset_root.join("python").join("python.exe");
    let mut found = None;
    run_python(&py_exe, &["--version"], |line| {
        if let Some(rest) = line.trim().strip_prefix("Python ") {
            found = Some(rest.trim().to_string());
        }
    })
    .ok()?;
    found
}

/// 版が違うと**言い切れる**ときだけ真。聞けなかったときは偽。
///
/// 記録の欠落は「更新ボタンを出す」だけで済むが、python の判定を誤ると
/// 「全部消して入れ直せ」という数 GB の要求になる。証拠が無い側へ倒す。
fn python_is_stale(installed: Option<&str>, pinned: Option<&str>) -> bool {
    matches!((installed, pinned), (Some(i), Some(p)) if i != p)
}

/// 実物に版を聞く必要があるか。
///
/// 記録が現在の pin を主張しているなら、それは初回導入が**実際に入れた**もの。
/// 聞き直さない（設定パネルを開くたびに python.exe を起動しないため）。
fn should_ask_python(recorded: &std::collections::BTreeMap<String, String>) -> bool {
    recorded.get("python") != current_pins().get("python")
}

/// 入れ直しが要る pin の名前を並べる。**python だけは記録ではなく実物で判定する。**
fn outdated_list(
    asset_root: &Path,
    recorded: &std::collections::BTreeMap<String, String>,
    recorded_reqs: &std::collections::BTreeMap<String, String>,
    recorded_models: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let current = current_pins();
    let mut out = outdated_pins(recorded, &current);
    // **固定 URL だけでは足りない** (v0.5.5 項目 3)。名前付き要件の版を変えても
    // `outdated` が空のままで、更新ボタンすら出なかった。
    // 欄が空の記録の読み方は `outdated_section` を参照。
    out.extend(outdated_section(
        recorded_reqs,
        &v054_baseline_requirements(),
        &current_requirements(),
    ));
    out.extend(outdated_section(
        recorded_models,
        &v054_baseline_models(),
        &current_models(),
    ));
    // 記録はいまのモデルを指しているのにファイルが欠けているもの（v0.5.7 項目 2。トークナイザなど）。
    // 欄が空の記録は基準値で読む（`outdated_section` と同じ）。
    let effective_models = if recorded_models.is_empty() {
        v054_baseline_models()
    } else {
        recorded_models.clone()
    };
    out.extend(incomplete_models(asset_root, &effective_models));
    out.sort();
    out.dedup();
    out.retain(|n| n != "python");
    if should_ask_python(recorded)
        && python_is_stale(
            installed_python_version(asset_root).as_deref(),
            pinned_python_version(),
        )
    {
        out.push("python".to_string());
        out.sort();
    }
    out
}

/// 導入状態を調べる (v0.5.4 項目 2)。
pub fn status(asset_root: &Path) -> IrodoriStatus {
    let present = assets_ready(asset_root);
    let Some(stamp) = read_stamp(asset_root) else {
        return IrodoriStatus {
            present,
            has_record: false,
            up_to_date: false,
            // **記録が無くても「入れ直せば済むもの」は名指しできる。** ここを空にすると
            // 呼び出し側が対象を自前で組み立てることになり、実際それで入れ直せない
            // python が混ざって、この機能が対象にしている環境がちょうど 1 つも
            // 更新できなくなっていた。
            outdated: if present {
                outdated_list(asset_root, &Default::default(), &Default::default(), &Default::default())
            } else {
                Vec::new()
            },
            resolved: Default::default(),
        };
    };
    // v0.5.4 以前が書いた記録には要件・モデルの欄が無い。基準値を書き足してから判定する。
    let stamp = backfill_baseline(asset_root, &stamp);
    let outdated = outdated_list(asset_root, &stamp.pins, &stamp.requirements, &stamp.models);
    IrodoriStatus {
        present,
        has_record: true,
        up_to_date: outdated.is_empty(),
        outdated,
        resolved: stamp.resolved,
    }
}

/// 要件・モデルの記録といまの要求を突き合わせる (v0.5.5 項目 3)。
///
/// **欄が空なら、その環境には v0.5.4 の基準値が入っているとみなす。** 欄が空なのは
/// v0.5.4 が書いた記録か、記録そのものが無い環境で、どちらも当時のビルドが入れた版が入っている。
///
/// - 固定 URL の 3 本と違い「欄が無い＝古い」とは言えない（言うと torch を含む数 GB の
///   再取得を強いる）
/// - **「欄が無い＝いまの要求どおり」とも言えない**（言うと要件を変えても届かない。
///   2026-09-14 監査で発覚）
///
/// どちらの証拠も無いので、**当時の固定値で読む**。欄があって名前が無いものは、あとから
/// 増えた要件なので対象にする（`outdated_pins` と同じ規則）。
fn outdated_section(
    recorded: &std::collections::BTreeMap<String, String>,
    baseline: &std::collections::BTreeMap<String, String>,
    current: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let effective = if recorded.is_empty() { baseline } else { recorded };
    outdated_pins(effective, current)
}

/// 欄が空の記録へ、**v0.5.4 の基準値**を書き込む (v0.5.5)。
///
/// 判定は書き込まなくても `outdated_section` が基準値で読むので正しい。書き込むのは、
/// 記録を「その環境に何が入っているか」の正本として完結させるため。
///
/// **いまの値ではなく固定の基準値を書く**（2026-09-14 監査で発覚）。以前は
/// `current_requirements()` を書いており、根拠は「v0.5.5 は要件を変えていない」という
/// コメントだけだった。要件を変えたビルドがここを通ると、新しい値を基準値として書いて
/// 差が出ず、**更新が永久に届かない**。
fn backfill_baseline(asset_root: &Path, stamp: &InstalledStamp) -> InstalledStamp {
    let out = baseline_filled(stamp, &v054_baseline_requirements(), &v054_baseline_models());
    let changed = out.requirements != stamp.requirements || out.models != stamp.models;
    if changed {
        let _ = write_stamp_pins(
            asset_root,
            out.pins.clone(),
            out.resolved.clone(),
            out.requirements.clone(),
            out.models.clone(),
        );
        crate::ulog!("[irodori] 導入記録に要件・モデルの基準値を書き足しました");
    }
    out
}

/// `backfill_baseline` の核（純粋関数）。**欄が空のときだけ**基準値で埋める。
///
/// 欄が 1 件でも埋まっている記録には触らない — それはその時のビルドが全部を記録したもので、
/// 名前が無いのは後から増えた要件だから（`outdated_section` が対象にする）。
fn baseline_filled(
    stamp: &InstalledStamp,
    baseline_requirements: &std::collections::BTreeMap<String, String>,
    baseline_models: &std::collections::BTreeMap<String, String>,
) -> InstalledStamp {
    let mut out = stamp.clone();
    if out.requirements.is_empty() {
        out.requirements = baseline_requirements.clone();
    }
    if out.models.is_empty() {
        out.models = baseline_models.clone();
    }
    out
}

fn outdated_pins(
    recorded: &std::collections::BTreeMap<String, String>,
    current: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    current
        .iter()
        .filter(|(name, url)| recorded.get(*name) != Some(url))
        .map(|(name, _)| name.clone())
        .collect()
}

pub fn assets_ready(asset_root: &Path) -> bool {
    let py = asset_root.join("python").join("python.exe");
    if !py.is_file() {
        return false;
    }
    let site = asset_root.join("python").join("Lib").join("site-packages");
    if !site.is_dir() {
        return false;
    }
    has_package(&site, "torch")
        && has_package(&site, "fastapi")
        && has_package(&site, "uvicorn")
        && has_package(&site, "huggingface_hub")
        && has_package(&site, "irodori_tts")
}

fn has_package(site_packages: &Path, name: &str) -> bool {
    // <pkg>/__init__.py または <pkg>.py または <pkg>-*.dist-info で判定
    let pkg = site_packages.join(name);
    if pkg.is_dir() {
        return true;
    }
    if site_packages.join(format!("{name}.py")).is_file() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(site_packages) else {
        return false;
    };
    let prefix = format!("{name}-");
    for e in entries.flatten() {
        if let Some(n) = e.file_name().to_str() {
            if n.starts_with(&prefix) && n.ends_with(".dist-info") {
                return true;
            }
        }
    }
    false
}

// ============ DL の各ステップ ============

/// 1) Embeddable Python の取得と展開。
/// 既に `python.exe` があれば skip する。
pub async fn ensure_python_embeddable<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_dir = asset_root.join("python");
    let py_exe = py_dir.join("python.exe");
    if py_exe.is_file() {
        on_line("Python ランタイムは既に配置済みです (skip)");
        return Ok(());
    }
    std::fs::create_dir_all(&py_dir)
        .with_context(|| format!("create python dir: {}", py_dir.display()))?;

    on_line(&format!(
        "Embeddable Python {PYTHON_VERSION} をダウンロードしています…"
    ));
    let zip_path = py_dir.join(format!("python-{PYTHON_VERSION}-embed-amd64.zip"));
    download_to(PYTHON_URL, &zip_path).await?;

    on_line("Python ZIP を展開しています…");
    expand_zip_windows(&zip_path, &py_dir)?;
    // 展開後の zip は不要
    let _ = std::fs::remove_file(&zip_path);

    on_line("python._pth を編集して site-packages を有効化…");
    enable_site_packages(&py_dir)?;

    Ok(())
}

/// 2) get-pip.py 経由で pip をブートストラップする。
pub async fn ensure_pip<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_dir = asset_root.join("python");
    let py_exe = py_dir.join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!("python.exe が見つかりません: {}", py_exe.display()));
    }
    let pip_dir = py_dir.join("Lib").join("site-packages").join("pip");
    if pip_dir.is_dir() {
        on_line("pip は既にブートストラップ済みです (skip)");
        return Ok(());
    }

    let get_pip = py_dir.join("get-pip.py");
    on_line("get-pip.py を取得しています…");
    download_to(GET_PIP_URL, &get_pip).await?;

    on_line("pip をブートストラップしています…");
    run_python(&py_exe, &[get_pip.to_string_lossy().as_ref()], |l| on_line(l))?;
    Ok(())
}

/// 3) 共通依存 (fastapi / uvicorn / huggingface_hub / numpy / soundfile) を pip install。
pub async fn install_common_requirements<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    on_line(&format!(
        "共通 Python 依存をインストールしています ({} パッケージ)…",
        COMMON_REQUIREMENTS.len()
    ));
    run_pip_install(&py_exe, COMMON_REQUIREMENTS, |l| on_line(l))?;
    Ok(())
}

/// 4) torch + torchaudio (CUDA 12.8) を pip install。サイズが大きい (1〜2GB)。
pub async fn install_torch_cuda<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    on_line(&format!("PyTorch (CUDA 12.8) をインストールしています…（{TORCH_DOWNLOAD}ダウンロードします）"));
    let mut args: Vec<&str> = vec!["--upgrade", "--index-url", TORCH_CUDA_INDEX_URL];
    args.extend(TORCH_PACKAGES);
    run_pip_install(&py_exe, &args, |l| on_line(l))?;
    Ok(())
}

/// 5) Irodori-TTS ランタイム本体 + 追加 pip 依存 (M4c Phase G)。
///
/// 順序: 追加 pip 依存 (transformers / peft / accelerate …) → silentcipher → dacvae → irodori-tts。
/// silentcipher/dacvae は irodori-tts の git+ 依存なので、irodori-tts を `--no-deps` で入れる前に
/// 別個に install しておく。dacvae リポジトリの公開状況や upstream API は実機 GPU で初回検証して
/// 必要に応じて URL/コミットを pin する想定 (本セッションは upstream pyproject 通りに繋ぐ)。
///
/// 全ステップに `--upgrade` を付け、過去に旧版が入っていた場合でも要件レンジに揃え直す。
pub async fn install_irodori_runtime<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!(
            "python.exe が見つかりません: {}",
            py_exe.display()
        ));
    }

    on_line(&format!(
        "Irodori-TTS の追加 pip 依存をインストールしています ({} パッケージ、~数百MB)…",
        IRODORI_EXTRA_REQUIREMENTS.len()
    ));
    let mut args: Vec<&str> = vec!["--upgrade"];
    args.extend(IRODORI_EXTRA_REQUIREMENTS);
    run_pip_install(&py_exe, &args, |l| on_line(l))?;

    on_line("silentcipher を GitHub アーカイブから取得しています…");
    run_pip_install(&py_exe, &["--no-deps", SILENTCIPHER_ZIPBALL], |l| on_line(l))?;

    on_line("dacvae を GitHub アーカイブから取得しています…");
    run_pip_install(&py_exe, &["--no-deps", DACVAE_ZIPBALL], |l| on_line(l))?;

    on_line("Irodori-TTS 本体を GitHub アーカイブから取得しています…");
    run_pip_install(&py_exe, &["--no-deps", IRODORI_TTS_ZIPBALL], |l| on_line(l))?;

    on_line("Irodori-TTS ランタイムのインストールが完了しました");
    Ok(())
}

/// 導入と更新を同時に走らせないための印（プロセスの中）。
///
/// **同じ `site-packages` を 2 つの経路が同時に触ると、退避 → 入れ直し → 復元の
/// どの段も守れない中間状態になる。** 記録が無い環境では「ランタイムをダウンロード」と
/// 「更新する」が両方押せるので、10〜20 分かかる初回 DL の途中で更新を押せてしまう。
/// 項目 3 の約束（失敗しても動いていた環境を壊さない）はこの排他が前提。
static IRODORI_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// いま導入または更新が走っているか (v0.5.5 項目 4)。
///
/// **合成の側もこれを見る。** 見ていなかったため、更新の最中に発話が来ると
/// **半分入れ替わった `site-packages` で新しいサイドカーが起動しうる**。
/// 数十秒だった v0.5.4 では踏みにくいが、数 GB・十数分になるモデル更新では現実的に踏む。
pub fn is_busy() -> bool {
    IRODORI_BUSY.load(std::sync::atomic::Ordering::SeqCst)
}

/// 導入・更新の錠のファイル（`asset_root` 直下。v0.5.6 項目 3f）。中身は使わない。消さない。
const UPDATE_LOCK_FILE: &str = "update.lock";

/// 導入・更新の錠を取り直す時間（`acquire_within`）。合成の側が「試してすぐ放す」一瞬を越えれば足りる。
const UPDATE_LOCK_WAIT: Duration = Duration::from_millis(500);

/// 取れたら作業してよい。drop で自動的に手放す（途中で return しても取り残さない）。
///
/// **二段の錠**（v0.5.6 項目 3f）: プロセスの中の印（`IRODORI_BUSY`）と、プロセスをまたぐファイルの錠
/// （`update.lock`）。順は プロセスの中 → ファイル。ファイルの錠はハンドル単位なので、一本にすると
/// 同じ ugg の 2 本目とも衝突し、「もう 1 つの ugg が動いています」と誤った案内になる。
pub struct IrodoriBusyGuard {
    /// `acquire_for` で取ったときだけ持つ。drop でファイルの錠も外れる。
    _cross_process: Option<crate::tts::file_lock::FileLock>,
}

impl IrodoriBusyGuard {
    /// プロセスの中の印だけを取る（テストと、プロセスの中だけで足りる用途）。
    pub fn acquire() -> Result<Self> {
        if IRODORI_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!(
                "Irodori ランタイムの導入または更新がすでに進行中です。終わってからもう一度お試しください"
            ));
        }
        Ok(Self {
            _cross_process: None,
        })
    }

    /// **プロセスをまたいで**取る（導入・更新のコマンドが使う。v0.5.6 項目 3f）。
    ///
    /// ファイルの錠が取れなければ、プロセスの中の印も手放して Err（もう 1 つの ugg が導入か更新をしている）。
    /// 錠のファイルを開けない環境は、導入・更新そのものも書き込めないので、同じく Err にする。
    pub fn acquire_for(asset_root: &Path) -> Result<Self> {
        // **同じ錠に積む**（別の錠を作って捨てると、捨てたほうの drop がプロセスの中の印を消す）。
        // Err で返るときはこの錠が drop され、プロセスの中の印も手放す。
        let mut guard = Self::acquire()?;
        // **少し待って取り直す**（v0.5.6 リリース前監査）。合成の側は錠を「試してすぐ放す」ので、その一瞬に
        // 重なると、空いている錠を「もう 1 つの ugg が更新している」と取り違えて断っていた。本当に更新している
        // ugg は分単位で握るので、0.5 秒待てば見分けられる。
        match crate::tts::file_lock::FileLock::acquire_within(
            &asset_root.join(UPDATE_LOCK_FILE),
            UPDATE_LOCK_WAIT,
        ) {
            Ok(Some(lock)) => {
                guard._cross_process = Some(lock);
                Ok(guard)
            }
            Ok(None) => Err(anyhow!(
                "もう 1 つの ugg が Irodori ランタイムの導入か更新をしています。そちらが終わるか、そちらを終了してから、もう一度お試しください"
            )),
            Err(err) => Err(anyhow!(
                "導入・更新の錠を取れません（{}）: {err}",
                asset_root.join(UPDATE_LOCK_FILE).display()
            )),
        }
    }
}

/// **この ugg かもう 1 つの ugg が**導入・更新をしているか（合成の側が見る。v0.5.6 項目 3f）。
///
/// プロセスの中の印が立っていればそれで足りる（ファイルの錠は自分が握っている）。立っていなければ、
/// ファイルの錠を試して**すぐ放す**。試すのは**プロセスの中で 1 本ずつ**にする — ファイルの錠は
/// ハンドル単位なので、掛け合いで 2 本同時にサイドカーを起こすと、片方が自分自身の試しに弾かれて
/// 理由なく VOICEVOX へ倒れる（反証レビュー #12）。
pub fn is_busy_for(asset_root: &Path) -> bool {
    if is_busy() {
        return true;
    }
    static PROBE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_at_a_time = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    crate::tts::file_lock::FileLock::is_held_elsewhere(&asset_root.join(UPDATE_LOCK_FILE))
}

impl Drop for IrodoriBusyGuard {
    fn drop(&mut self) {
        IRODORI_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `IRODORI_BUSY` を取る・見るテストどうしを直列にする（2026-09-14 監査で発覚）。
///
/// `IRODORI_BUSY` はプロセス全体で 1 つで、cargo test はテストを並列に走らせる。奪い合うと
/// `acquire().unwrap()` の panic や、busy のはずが空いている（その逆）の assert 失敗がたまに起きる。
#[cfg(test)]
pub(crate) fn lock_busy_for_test() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 更新の作業用ディレクトリ（site-packages の**外**に置く）。
///
/// site-packages の中に退避すると、名前次第で import されうるうえ、pip が
/// dist-info を拾って混乱する。`asset_root` 直下に置いて完全に切り離す。
const UPDATE_BACKUP_DIR: &str = ".update-backup";

/// pin ごとの「入れ直し方」。
///
/// **`python` はここに無い。** `ensure_python_embeddable` は `python.exe` があれば
/// skip するため、Python 本体の pin を上げても既存環境には反映されない。
/// 稼働中のインタプリタをその場で差し替える安全な方法は無いので、
/// **入れ直しの対象にせず、全体の入れ直しが要る旨を伝える**（黙って失敗させない）。
fn updatable_pin(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        // (site-packages 上のディレクトリ名, 入れ直しに使う URL)
        "silentcipher" => Some(("silentcipher", SILENTCIPHER_ZIPBALL)),
        "dacvae" => Some(("dacvae", DACVAE_ZIPBALL)),
        "irodori_tts" => Some(("irodori_tts", IRODORI_TTS_ZIPBALL)),
        _ => None,
    }
}

/// 資産ルートから site-packages の位置を出す。
fn site_of(asset_root: &Path) -> PathBuf {
    asset_root.join("python").join("Lib").join("site-packages")
}

/// site-packages にいま何があるかを 1 行で述べる（実機検証の観測点）。
///
/// pip が `Successfully installed` と言っているのに**ファイルが変わっていない**
/// ことがあったため、入れ直しの前後で実際の姿を証跡に残す。
/// `direct_url.json` の `url` は「どの版か」を示す唯一の手がかりになる。
fn describe_package(site: &Path, pkg: &str) -> String {
    let mut infos: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(site) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{pkg}-")) && name.ends_with(".dist-info") {
                let url = std::fs::read_to_string(e.path().join("direct_url.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(str::to_string))
                    .unwrap_or_else(|| "(url なし)".to_string());
                infos.push(format!("{name} <- {url}"));
            }
        }
    }
    format!("dir={} info=[{}]", site.join(pkg).is_dir(), infos.join(", "))
}

/// `site-packages/<pkg>` と `<pkg>-*.dist-info` を退避先へ移す。
fn move_package_aside(site: &Path, pkg: &str, backup: &Path) -> Result<()> {
    std::fs::create_dir_all(backup).with_context(|| format!("mkdir {}", backup.display()))?;
    let mut moved = false;
    let dir = site.join(pkg);
    if dir.is_dir() {
        std::fs::rename(&dir, backup.join(pkg))
            .with_context(|| format!("退避: {}", dir.display()))?;
        moved = true;
    }
    // dist-info はバージョン番号を含むので走査して拾う。
    if let Ok(entries) = std::fs::read_dir(site) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{pkg}-")) && name.ends_with(".dist-info") {
                std::fs::rename(e.path(), backup.join(&name))
                    .with_context(|| format!("退避: {name}"))?;
                moved = true;
            }
        }
    }
    if !moved {
        // 入っていなかった場合も更新自体は続行してよい（新規に入る）。
        crate::ulog!("[irodori] 退避対象が見つかりません (新規導入として続行): {pkg}");
    }
    Ok(())
}

/// 退避したものを元の場所へ戻す。
fn restore_package(site: &Path, backup: &Path) -> Result<()> {
    let entries = std::fs::read_dir(backup)
        .with_context(|| format!("退避先の読み取り: {}", backup.display()))?;
    for e in entries.flatten() {
        let dest = site.join(e.file_name());
        // 失敗した入れ直しが中途半端に残していたら先に退ける。
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(e.path(), &dest)
            .with_context(|| format!("復元: {}", dest.display()))?;
    }
    Ok(())
}

/// 入れ直しで壊れていないかを見るモジュール（import 名）。
const RUNTIME_MODULES: &[&str] = &["irodori_tts", "dacvae", "silentcipher"];

/// いま import できるか。できなければ**理由**（例外の最終行）を添える。
///
/// 理由を捨ててはいけない。最初の実装は出力を握り潰しており、実機で落ちたときに
/// 分かったのは「python 異常終了 (code Some(1))」だけだった。原因
/// （`No module named 'pydub'`）に辿り着くのに余計な一往復を要した。
///
/// 理由の取り方は `failure_reason`。失敗は想定内（`silentcipher` は一度も import できていない）
/// なので、ログには残さない。
fn import_report(py_exe: &Path) -> std::collections::BTreeMap<String, Option<String>> {
    RUNTIME_MODULES
        .iter()
        .map(|m| {
            let code = format!("import {m}");
            (
                m.to_string(),
                failure_reason(py_exe, &["-c", code.as_str()]),
            )
        })
        .collect()
}

/// 走らせて、失敗したら**理由**（stderr の最後の行）を返す。成功なら `None`。
///
/// **理由は stderr の最後の行から取る**（v0.5.6 項目 2）。出力を行ごとに流すようになり、
/// stdout と stderr は届いた順に混ざる。import の途中で stdout に書いたものは終了時に
/// まとめて吐き出されるので、**両方の最後の行を取ると、例外の行ではなくそちらを拾いうる**。
/// 進捗の上書きの行も理由にしない。
fn failure_reason(exe: &Path, args: &[&str]) -> Option<String> {
    let mut last = None;
    run_python_lines(exe, args, false, |l| {
        if l.stream == Stream::Stderr && !l.overwritten {
            last = Some(l.text.to_string());
        }
    })
    .err()
    .map(|err| last.unwrap_or_else(|| format!("{err:#}")))
}

/// **入れ直す前より悪くなったものだけ**を返す。
///
/// **「全部 import できること」を条件にしてはいけない。** 実機の `silentcipher` は
/// `pydub` が入っていないため**一度も import できたことが無い**（ugg は `pydub` を
/// 入れず、upstream の `watermark.py` も `ImportError` を握って「透かし無しで続行」する
/// 設計）。それでも合成は成立している。ここで「全部使えること」を絶対条件にすると、
/// **既存環境では必ずロールバックし、更新が誰にも一度も成功しない**
/// （2026-09-11 の実機検証で実際にそうなった）。
///
/// 項目 3 が約束しているのは「失敗しても、それまで動いていた環境を壊さない」であって
/// 「壊れていた環境を直す」ではない。したがって判定は**絶対値ではなく差分**で行う。
fn import_regressions(
    before: &std::collections::BTreeMap<String, Option<String>>,
    after: &std::collections::BTreeMap<String, Option<String>>,
) -> Vec<String> {
    before
        .iter()
        .filter(|(_, err)| err.is_none())
        .filter_map(|(m, _)| match after.get(m) {
            Some(None) => None,
            Some(Some(why)) => Some(format!("{m}: {why}")),
            None => Some(m.clone()),
        })
        .collect()
}

/// 入れ直せた分だけを現在値へ反映した記録用の `pins` を作る。
///
/// **全部を現在値にしてはいけない。** 入れ直していない依存まで「最新」と記録すると、
/// 記録そのものが嘘になる。`python` は入れ直さないが、**実物が pin と一致していることを
/// 確認できたときだけ**記録する（確認せずに書けば嘘になり、確認したのに書かなければ
/// 設定パネルを開くたびに `python.exe` に聞き直すことになる）。
fn merged_pins(
    recorded: &std::collections::BTreeMap<String, String>,
    updated: &[String],
    python_matches: bool,
) -> std::collections::BTreeMap<String, String> {
    let current = current_pins();
    let mut pins = recorded.clone();
    for name in updated {
        if let Some(url) = current.get(name) {
            pins.insert(name.clone(), url.clone());
        }
    }
    if python_matches {
        if let Some(url) = current.get("python") {
            pins.insert("python".to_string(), url.clone());
        }
    }
    pins
}

/// その配布は torch の CUDA index から入れる必要があるか (v0.5.5 項目 3)。
///
/// **ここを外すと PyPI の CPU 版が入り、GPU 合成が黙って壊れる。**
/// 初回導入は `install_torch_cuda` が `--index-url` を付けているので、入れ直しでも揃える。
fn needs_torch_index(name: &str) -> bool {
    TORCH_PACKAGES
        .iter()
        .any(|spec| requirement_name(spec) == name)
}

/// 全配布の版を聞くときの目印（v0.5.6 項目 3d）。記録用の `VERSIONS_MARKER` とは別。
const ALL_VERSIONS_MARKER: &str = "UGG_ALL_VERSIONS ";

/// 更新の前に控える、全配布の版（v0.5.6 項目 3d）。
///
/// **退避のディレクトリ（`.update-backup`）の外に置く。** 中に置くと、次の更新の冒頭の「退避が残って
/// いたら先に戻す」処理がこのファイルを退避として扱い、失敗して**以後ずっと更新できなくなる**。
const VERSIONS_SNAPSHOT_FILE: &str = "update-versions.json";

/// その他の要件を入れるときに torch を縛る制約ファイル（入れ終わったら消す）。
const TORCH_CONSTRAINTS_FILE: &str = "update-constraints.txt";

/// GitHub の zipball で入れている 3 本の配布名（正規化した名前）。
///
/// **版を指定して入れ直さない。** PyPI には無い（同名の別物がありうる）ので、`dacvae==x` を入れると
/// 失敗するか**別のパッケージが入る**。この 3 本は退避のディレクトリから戻す。
const PINNED_DISTRIBUTIONS: &[&str] = &["silentcipher", "dacvae", "irodori-tts"];

/// 固定 URL の 3 本を入れる順（依存の順。初回導入と同じ）。
const PIN_ORDER: &[&str] = &["silentcipher", "dacvae", "irodori_tts"];

/// 配布名を正規化する（PEP 503: 小文字にし、`-` `_` `.` の並びを `-` 1 つにする）。
/// `importlib.metadata` が返す名前は書き方がまちまちなので、比べる前に揃える。
fn normalize_dist_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
            continue;
        }
        if sep && !out.is_empty() {
            out.push('-');
        }
        sep = false;
        out.extend(c.to_lowercase());
    }
    out
}

/// pip が入れ替えの途中で止まったときに残す一時退避か（名前が `~` で始まる）。
///
/// pip は古い版を `~ransformers` / `~ransformers-4.57.6.dist-info` のように先頭を `~` に変えた名前へ移してから
/// 新しい版を入れ、済んだら消す。**途中でプロセスごと止まると残り**、`importlib.metadata.distributions()` は
/// これも `Name: transformers` の配布として返す（v0.5.7 の実機検証 E-11 で、dev の再起動で中断した更新が残した）。
fn is_pip_stash(name: &str) -> bool {
    name.starts_with('~')
}

/// `ALL_VERSIONS_MARKER` の行から、全配布の版（名前は正規化済み）を取り出す。
///
/// 行は `[名前, 版, dist-info のディレクトリ名]` の並び。**pip の一時退避（`is_pip_stash`）は数えない。**
/// 数えると、入れ替えの途中で止まった配布が同じ名前で 2 つ並び、名前で畳んだときに一時退避の古い版が
/// 残りうる — 実際に「transformers は控えと同じ 4.57.6」と読み、5.17.0 のコードを残したまま依存だけ
/// 旧版へ戻して、v3 が import できなくなった（E-11）。
fn parse_all_versions_line(line: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let json = line.trim().strip_prefix(ALL_VERSIONS_MARKER)?;
    let raw: Vec<(String, String, String)> = serde_json::from_str(json).ok()?;
    Some(
        raw.into_iter()
            .filter(|(_, _, dir)| !is_pip_stash(dir))
            .map(|(name, version, _)| (normalize_dist_name(&name), version))
            .collect(),
    )
}

/// site-packages に残った pip の一時退避（`is_pip_stash`）を消し、消した名前を返す。
///
/// **控えの版へ全部戻せたあとにだけ呼ぶ**（`recover_interrupted_update`）。戻したあとの一時退避は古い版の抜け殻で、
/// 使われないのに pip が呼ばれるたびに `Ignoring invalid distribution` と警告し、ディスクを百 MB 単位で塞ぐ。
fn remove_pip_stash(site: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(site) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_pip_stash(&name) {
            continue;
        }
        let path = entry.path();
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match gone {
            Ok(()) => removed.push(name),
            Err(err) => crate::ulog!("[irodori] pip の一時退避を消せませんでした（残します）: {}: {err}", path.display()),
        }
    }
    removed.sort();
    removed
}

/// いま入っている**全部の**配布の版を聞く（v0.5.6 項目 3d の控え）。
///
/// **記録用の `query_resolved_versions` を使ってはいけない。** あちらは ugg が名指しで入れた約 24 件しか
/// 見ないので、pip が依存を連鎖して入れ替える `tokenizers` などが控えに入らない。見えていない配布は
/// 差分ゼロに見え、**戻していないのに「戻しました」と言う**ことになる。
fn query_all_versions<F>(py_exe: &Path, mut on_line: F) -> Result<std::collections::BTreeMap<String, String>>
where
    F: FnMut(&str),
{
    let script = format!(
        "import json,importlib.metadata as m
out=[]
for d in m.distributions():
    n=d.metadata.get('Name')
    if n: out.append([n,d.version,getattr(getattr(d,'_path',None),'name','')])
print({marker:?}+json.dumps(out))",
        marker = ALL_VERSIONS_MARKER,
    );
    let mut found = None;
    run_python(py_exe, &["-c", &script], |line| match parse_all_versions_line(line) {
        Some(map) => found = Some(map),
        None => on_line(line),
    })?;
    found.ok_or_else(|| anyhow!("入っている版の一覧を読み取れませんでした"))
}

/// 控えの版と違う（または消えた）配布。**固定 URL の 3 本は含めない**（退避から戻すため）。
/// 新しく増えた配布は含めない（残しても import されなければ使われない。`added_since` でログにだけ出す）。
fn versions_to_restore(
    before: &std::collections::BTreeMap<String, String>,
    after: &std::collections::BTreeMap<String, String>,
) -> Vec<(String, String)> {
    before
        .iter()
        .filter(|(name, _)| !PINNED_DISTRIBUTIONS.contains(&name.as_str()))
        .filter(|(name, version)| after.get(*name) != Some(*version))
        .map(|(name, version)| (name.clone(), version.clone()))
        .collect()
}

/// 控えに無かった（更新で新しく入った）配布。
fn added_since(
    before: &std::collections::BTreeMap<String, String>,
    after: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .cloned()
        .collect()
}

/// `name==version` の並びを、torch の CUDA index から入れるものとそれ以外に分ける。
///
/// **1 回の pip にまとめてはいけない。** `--index-url` は呼び出し全体に効くので、CUDA の index に無い
/// パッケージが取れない。逆に torch 系を PyPI から取ると **CPU 版**が入り、GPU 合成が黙って壊れる。
fn split_by_index(items: &[(String, String)]) -> (Vec<String>, Vec<String>) {
    let mut torch = Vec::new();
    let mut other = Vec::new();
    for (name, version) in items {
        let spec = format!("{name}=={version}");
        if needs_torch_index(name) {
            torch.push(spec);
        } else {
            other.push(spec);
        }
    }
    (torch, other)
}

/// CUDA 版の torch が CPU 版へ入れ替わったか（v0.5.6 項目 3c の守り）。
///
/// CUDA の index の torch は版に `+cu128` のような印が付き、Windows の PyPI の torch（CPU 版）には付かない。
/// 依存の連鎖で PyPI の torch が入ると、合成は GPU を使えなくなり、**無言で VOICEVOX へ落ちる**。
fn cuda_torch_replaced(
    before: &std::collections::BTreeMap<String, String>,
    after: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    let was_cuda = before.get("torch").is_some_and(|v| v.contains("+cu"));
    match after.get("torch") {
        _ if !was_cuda => None,
        Some(v) if v.contains("+cu") => None,
        Some(v) => Some(format!("torch が CUDA 版から CPU 版（{v}）へ入れ替わりました")),
        None => Some("torch が消えました".to_string()),
    }
}

fn versions_snapshot_path(asset_root: &Path) -> PathBuf {
    asset_root.join(VERSIONS_SNAPSHOT_FILE)
}

/// 控えを書く（書きかけを読ませないよう、差し替えで書く）。
fn write_versions_snapshot(
    asset_root: &Path,
    versions: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let path = versions_snapshot_path(asset_root);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(versions).context("版の控えの JSON 化")?;
    std::fs::write(&tmp, json).with_context(|| format!("版の控えの書き出し: {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("版の控えの差し替え: {}", path.display()))?;
    Ok(())
}

fn read_versions_snapshot(asset_root: &Path) -> Option<std::collections::BTreeMap<String, String>> {
    let text = std::fs::read_to_string(versions_snapshot_path(asset_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn remove_versions_snapshot(asset_root: &Path) {
    let _ = std::fs::remove_file(versions_snapshot_path(asset_root));
}

/// 名前付きの配布を、控えの版へ入れ直す（v0.5.6 項目 3d）。**戻せなかったもの**を返す。
///
/// - `--no-deps` で、変わった配布を**全部まとめて**控えの版に固定して入れ直す（依存を解決させると、
///   また別の版を連れてくる）
/// - torch 系とそれ以外で pip を分ける（`split_by_index`）
/// - 入れ直すには**通信が要る**。PyPI からその版が消えていれば戻せない（戻せなかったものとして返す）
fn roll_back_versions<F>(
    py_exe: &Path,
    before: &std::collections::BTreeMap<String, String>,
    mut on_line: F,
) -> Result<Vec<String>>
where
    F: FnMut(&str),
{
    let after = query_all_versions(py_exe, |l| on_line(l))?;
    let added = added_since(before, &after);
    if !added.is_empty() {
        // 消さない（import されなければ使われない。消すほうが別の依存を壊しうる）。
        crate::ulog!("[irodori] 更新で新しく入った配布は残します: {}", added.join(", "));
    }
    let to_restore = versions_to_restore(before, &after);
    if to_restore.is_empty() {
        return Ok(Vec::new());
    }
    on_line(&format!(
        "入れ替わった {} 件を元の版へ戻しています…（通信が要ります）",
        to_restore.len()
    ));
    let (torch, other) = split_by_index(&to_restore);
    if !torch.is_empty() {
        let mut args: Vec<&str> = vec!["--no-deps", "--force-reinstall", "--index-url", TORCH_CUDA_INDEX_URL];
        args.extend(torch.iter().map(String::as_str));
        if let Err(err) = run_pip_install(py_exe, &args, |l| on_line(l)) {
            on_line(&format!("PyTorch を元の版へ戻せませんでした: {err:#}"));
        }
    }
    if !other.is_empty() {
        let mut args: Vec<&str> = vec!["--no-deps", "--force-reinstall"];
        args.extend(other.iter().map(String::as_str));
        if let Err(err) = run_pip_install(py_exe, &args, |l| on_line(l)) {
            on_line(&format!("元の版へ戻せない配布があります: {err:#}"));
        }
    }
    let now = query_all_versions(py_exe, |l| on_line(l))?;
    Ok(versions_to_restore(before, &now)
        .into_iter()
        .map(|(name, version)| match now.get(&name) {
            Some(v) => format!("{name}=={version}（いまは {v}）"),
            None => format!("{name}=={version}（いまは入っていない）"),
        })
        .collect())
}

/// 退避が**最後まで**済んだ印（`<退避先>/<pkg>.aside-complete`）。
///
/// 戻すとき、退避が済んでいれば「いま site-packages にあるその配布」は入れ直しで入った新しいもの
/// なので、先に退けてから戻す（残すと dist-info が 2 つになる）。**済んでいなければ、残っているのは
/// 動かせなかった原本**なので、消してはいけない。
fn aside_complete_marker(backup_root: &Path, pkg: &str) -> PathBuf {
    backup_root.join(format!("{pkg}.aside-complete"))
}

/// いま site-packages にある `<pkg>` と `<pkg>-*.dist-info` を消す（退避を戻す前の片付け）。
fn remove_installed_package(site: &Path, pkg: &str) -> Result<()> {
    let dir = site.join(pkg);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("片付け: {}", dir.display()))?;
    }
    if let Ok(entries) = std::fs::read_dir(site) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{pkg}-")) && name.ends_with(".dist-info") {
                std::fs::remove_dir_all(e.path()).with_context(|| format!("片付け: {name}"))?;
            }
        }
    }
    Ok(())
}

/// 退避を全部戻す（固定 URL の 3 本。通信は要らない）。**ディレクトリだけ**を退避として扱う
/// （印や控えのファイルを `restore_package` に渡すと、読み取りに失敗して「戻せない」と誤判定する）。
fn restore_all_backups(site: &Path, backup_root: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(backup_root) else {
        return Ok(());
    };
    let mut failed: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let pkg = e.file_name().to_string_lossy().into_owned();
        let result = (|| {
            if aside_complete_marker(backup_root, &pkg).is_file() {
                remove_installed_package(site, &pkg)?;
            }
            restore_package(site, &path)
        })();
        if let Err(err) = result {
            crate::ulog!("[irodori] 退避の復元に失敗: {} ({err:#})", path.display());
            failed.push(pkg);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("退避を戻せません: {}", failed.join(", ")))
    }
}

/// 初回導入（全部の入れ直し）が成功したあと、前回の更新の残り（退避と版の控え）を片付ける（v0.5.6 リリース前監査）。
///
/// 残るのは、更新の全戻しが通信の無さなどで失敗したとき。そのあと「ランタイムをダウンロード」で入れ直すと、記録は
/// 最新になるのに残りは消えず、**次に更新が出た回の入口（`recover_interrupted_update`）が、入れたばかりのものを
/// 古い退避と控えで書き戻していた**。全部入れ直せたので、残りはもう戻す先ではない。片付けたら真を返す。
pub fn discard_update_leftovers<F>(asset_root: &Path, mut on_line: F) -> bool
where
    F: FnMut(&str),
{
    let backup_root = asset_root.join(UPDATE_BACKUP_DIR);
    let snapshot = versions_snapshot_path(asset_root);
    if !backup_root.exists() && !snapshot.exists() {
        return false;
    }
    let _ = std::fs::remove_dir_all(&backup_root);
    remove_versions_snapshot(asset_root);
    on_line("前回の更新の残り（退避と版の控え）を片付けました（全部入れ直したので、もう使いません）");
    crate::ulog!("[irodori] 初回導入が済んだので、前回の更新の残りを片付けました: {}", backup_root.display());
    true
}

/// 前回の更新が途中で止まっていたら、先に元へ戻す（v0.5.4 項目 3 / v0.5.6 項目 3d）。
///
/// **残っている退避や控えを無条件に消さない。** 残っているのは「戻すのに失敗したので消さずに置いた」か
/// 「途中でアプリが落ちた」ときだけで、そこには**唯一残った旧版の手がかり**が入っている。
/// 戻せたら捨てる。戻せなければ場所を伝えて止まる。
fn recover_interrupted_update<F>(asset_root: &Path, py_exe: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let site = site_of(asset_root);
    let backup_root = asset_root.join(UPDATE_BACKUP_DIR);
    if backup_root.is_dir() {
        on_line("前回の入れ直しが中断しています。退避したものを先に戻します…");
        restore_all_backups(&site, &backup_root).with_context(|| {
            format!(
                "前回の入れ直しで退避したものを戻せません。手動で戻してから再実行してください。退避先: {}",
                backup_root.display()
            )
        })?;
        let _ = std::fs::remove_dir_all(&backup_root);
        on_line("戻しました");
    }
    if let Some(before) = read_versions_snapshot(asset_root) {
        on_line("前回の更新が途中で止まっています。入れ替わった依存を元の版へ戻します…");
        let left = roll_back_versions(py_exe, &before, |l| on_line(l))
            .context("前回の更新の後始末で、入っている版を確かめられませんでした")?;
        if !left.is_empty() {
            return Err(anyhow!(
                "前回の更新で入れ替わった依存を元の版へ戻せません（通信を確かめてから、もう一度更新してください）: {}。控え: {}",
                left.join(" / "),
                versions_snapshot_path(asset_root).display()
            ));
        }
        // 前回の更新が pip の入れ替えの途中で止まっていれば、pip の一時退避が残っている（E-11）。全部戻せたので抜け殻
        let stash = remove_pip_stash(&site);
        if !stash.is_empty() {
            on_line(&format!("止まった pip が残した一時退避を片付けました: {}", stash.join(", ")));
            crate::ulog!("[irodori] 止まった pip が残した一時退避を片付けました: {}", stash.join(", "));
        }
        remove_versions_snapshot(asset_root);
        on_line("戻しました");
    }
    Ok(())
}

/// 何をどの順で入れ直すか（v0.5.6 項目 3c。純関数）。
///
/// **`outdated` の並び（アルファベット順）に従わない。** 順は ① 名前付き要件（torch 系を先に）
/// ② 固定 URL の 3 本（依存の順）③ モデル ④ 合成で確かめる。
#[derive(Debug, Default, PartialEq, Eq)]
struct UpdatePlan {
    /// torch の CUDA index から入れる要件（`current_requirements()` の書き方のまま）。
    torch: Vec<String>,
    /// それ以外の名前付き要件。**1 回の pip** で入れる（依存の解決を 1 回にする）。
    other: Vec<String>,
    /// 固定 URL の 3 本（`PIN_ORDER` の順）。
    pins: Vec<&'static str>,
    /// モデルを確かめて取るか（`--download-only` が 3 本まとめて etag を照合する）。
    models: bool,
    /// 入れ直すもの全部（記録に使う）。
    names: Vec<String>,
    /// 入れ直せないので飛ばすもの。
    skipped: Vec<String>,
}

impl UpdatePlan {
    /// site-packages を入れ替えるか（版の控えが要るか）。
    fn touches_packages(&self) -> bool {
        !self.torch.is_empty() || !self.other.is_empty() || !self.pins.is_empty()
    }
}

fn update_plan(outdated: &[String]) -> UpdatePlan {
    let requirements = current_requirements();
    let models = current_models();
    let mut plan = UpdatePlan::default();
    for name in outdated {
        if updatable_pin(name).is_some() {
            plan.names.push(name.clone());
        } else if models.contains_key(name) {
            plan.models = true;
            plan.names.push(name.clone());
        } else if let Some(spec) = requirements.get(name) {
            if needs_torch_index(name) {
                plan.torch.push(spec.clone());
            } else {
                plan.other.push(spec.clone());
            }
            plan.names.push(name.clone());
        } else {
            plan.skipped.push(name.clone());
        }
    }
    plan.pins = PIN_ORDER
        .iter()
        .copied()
        .filter(|pkg| outdated.iter().any(|n| n == pkg))
        .collect();
    plan
}

/// その他の要件を入れるときに、torch をいまの版に縛る制約（v0.5.6 項目 3c）。
///
/// 依存の解決で torch の版が動くと、PyPI から **CPU 版**を取りに行く（その pip には CUDA の index を
/// 渡していない）。いまの版に縛っておけば、動かす必要があるときは pip が**入れる前に**失敗する。
fn torch_constraints(versions: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    TORCH_PACKAGES
        .iter()
        .map(|spec| normalize_dist_name(requirement_name(spec)))
        .filter_map(|name| versions.get(&name).map(|v| format!("{name}=={v}")))
        .collect()
}

/// 計画どおりに入れ直す（失敗したら Err。**戻すのは呼び出し側**）。
async fn apply_update_plan<F>(
    asset_root: &Path,
    py_exe: &Path,
    plan: &UpdatePlan,
    before_imports: &std::collections::BTreeMap<String, Option<String>>,
    before_versions: &std::collections::BTreeMap<String, String>,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(&str),
{
    let site = site_of(asset_root);
    let backup_root = asset_root.join(UPDATE_BACKUP_DIR);
    let regressions = |on_line: &mut F| -> Result<()> {
        let regressed = import_regressions(before_imports, &import_report(py_exe));
        if regressed.is_empty() {
            Ok(())
        } else {
            on_line(&format!("入れ直したことで import できなくなりました: {}", regressed.join(" / ")));
            Err(anyhow!(
                "入れ直したことで import できなくなりました: {}",
                regressed.join(" / ")
            ))
        }
    };

    // ⓪ 透かしの重みを先に取る（v0.5.7 項目 5）。名前付き要件（pydub を含む）より前に置く — 途中で全戻し
    //    しても pydub は残るので、残ったときに合成で黙って取りに行かせない。失敗しても止めない。
    if let Err(err) = prefetch_watermark_weights(asset_root, |l| on_line(l)) {
        report_watermark_prefetch_failure(&err, |l| on_line(l));
    }

    // ① 名前付き要件。torch 系を先に（その他の要件が新しい torch を前提にしていても、PyPI から
    //    取りに行かせないため）。
    if !plan.torch.is_empty() {
        on_line(&format!("PyTorch を入れ直しています…（CUDA 12.8 の index から。{TORCH_DOWNLOAD}あります）"));
        let mut args: Vec<&str> = vec!["--upgrade", "--index-url", TORCH_CUDA_INDEX_URL];
        args.extend(plan.torch.iter().map(String::as_str));
        run_pip_install(py_exe, &args, |l| on_line(l)).context("PyTorch の入れ直しに失敗しました")?;
    }
    if !plan.other.is_empty() {
        on_line(&format!(
            "Python の依存を入れ直しています…（{}）",
            plan.other.join(" ")
        ));
        let now = query_all_versions(py_exe, |l| on_line(l))?;
        let constraints = torch_constraints(&now);
        let constraints_path = asset_root.join(TORCH_CONSTRAINTS_FILE);
        let mut args: Vec<String> = vec!["--upgrade".to_string()];
        if !constraints.is_empty() {
            std::fs::write(&constraints_path, constraints.join("\n") + "\n")
                .with_context(|| format!("制約ファイルの書き出し: {}", constraints_path.display()))?;
            args.push("-c".to_string());
            args.push(constraints_path.to_string_lossy().into_owned());
        }
        args.extend(plan.other.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let installed = run_pip_install(py_exe, &refs, |l| on_line(l));
        let _ = std::fs::remove_file(&constraints_path);
        installed.context("Python の依存の入れ直しに失敗しました")?;
    }
    if !plan.torch.is_empty() || !plan.other.is_empty() {
        let now = query_all_versions(py_exe, |l| on_line(l))?;
        if let Some(why) = cuda_torch_replaced(before_versions, &now) {
            return Err(anyhow!("{why}（GPU で合成できなくなるため、元に戻します）"));
        }
        regressions(&mut on_line)?;
    }

    // ② 固定 URL の 3 本（依存の順）。退避してから入れ直す（戻すのは呼び出し側）。
    for pkg in &plan.pins {
        let Some((pkg, url)) = updatable_pin(pkg) else {
            continue;
        };
        on_line(&format!("{pkg} を入れ直しています…"));
        on_line(&format!("  site={}", site.display()));
        on_line(&format!("  退避前: {}", describe_package(&site, pkg)));
        let backup = backup_root.join(pkg);
        // 退避は「ディレクトリ」と「dist-info」の 2 段で、前者だけ動いて後者で失敗しうる
        // （ファイルがロックされている等）。**そのまま返すと site-packages から消えたまま**
        // になるので、ここで戻す（印を書く前なので、あとの全戻しは原本を消さない）。
        if let Err(err) = move_package_aside(&site, pkg, &backup) {
            if let Err(restore_err) = restore_package(&site, &backup) {
                crate::ulog!(
                    "[irodori] 退避中の失敗を戻せません。退避を残します: {} ({restore_err:#})",
                    backup.display()
                );
            }
            return Err(err);
        }
        std::fs::write(aside_complete_marker(&backup_root, pkg), b"")
            .with_context(|| format!("退避の印: {}", backup.display()))?;
        on_line(&format!("  退避後: {}", describe_package(&site, pkg)));
        run_pip_install(
            py_exe,
            &[
                "--no-deps",
                // 直 URL でも確実に入れ替えるため、キャッシュと既存判定を跨がせない。
                "--force-reinstall",
                url,
            ],
            |l| on_line(l),
        )
        .with_context(|| format!("{pkg} の入れ直しに失敗しました"))?;
        on_line(&format!("  入れ直し後: {}", describe_package(&site, pkg)));
        regressions(&mut on_line)?;
    }

    // ③ モデル（`--download-only` が 3 本まとめて etag を照合し、差があるときだけ取る）。
    if plan.models {
        on_line("HF モデルを確認しています…（差があるときだけ取得します）");
        let sidecar_py = asset_root.join("sidecar.py");
        install_irodori_models(asset_root, &sidecar_py, |l| on_line(l))
            .await
            .context("HF モデルの取得に失敗しました")?;
    }

    // ④ 合成で確かめる のは呼び出し側（`update_irodori_runtime`。不合格なら戻したあとにもう一度試すため）。
    Ok(())
}

/// 失敗した更新を元に戻し、**何が戻って何が戻らなかったか**を添えたエラーを返す（v0.5.6 項目 3d）。
///
/// 戻す強さは 3 種類で違う: 固定 URL の 3 本は退避から（通信は要らない）／名前付きの配布は控えの版へ
/// 入れ直す（**通信が要り、戻せないことがある**）／モデルは戻さない（同じ repo@revision なら上書き。
/// v0.5.6 は版を変えないので差は出ない）。戻せなかったものがあれば**退避と控えを残し**、次の更新の前に
/// もう一度戻す。
fn roll_back_update<F>(
    asset_root: &Path,
    py_exe: &Path,
    before_versions: Option<&std::collections::BTreeMap<String, String>>,
    err: anyhow::Error,
    mut on_line: F,
) -> anyhow::Error
where
    F: FnMut(&str),
{
    on_line(&format!("更新に失敗しました。入れ替えたものを元に戻します: {err:#}"));
    let site = site_of(asset_root);
    let backup_root = asset_root.join(UPDATE_BACKUP_DIR);
    let mut left: Vec<String> = Vec::new();
    if backup_root.is_dir() {
        match restore_all_backups(&site, &backup_root) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&backup_root);
            }
            Err(e) => left.push(format!("{e:#}（退避先: {}）", backup_root.display())),
        }
    }
    if let Some(before) = before_versions {
        match roll_back_versions(py_exe, before, |l| on_line(l)) {
            Ok(rest) => left.extend(rest),
            Err(e) => left.push(format!("入っている版を確かめられません: {e:#}")),
        }
    }
    if left.is_empty() {
        remove_versions_snapshot(asset_root);
        on_line("元の状態へ戻しました");
        anyhow!("{err:#}（入れ替えたものは元の版へ戻しました）")
    } else {
        on_line(&format!("元に戻せなかったもの: {}", left.join(" / ")));
        anyhow!(
            "{err:#}。元に戻せなかったもの: {}（退避と控えは残してあり、次の更新の前にもう一度戻します）",
            left.join(" / ")
        )
    }
}

// ============ 1 回合成のゲート (v0.5.6 項目 3b) ============

/// 一発合成の報告の目印（`sidecar.py` の `SYNTH_ONCE_*_MARKER` と同じ文字列。契約テストが見張る）。
const SYNTH_ONCE_START_MARKER: &str = "UGG_SYNTH_ONCE_START ";
const SYNTH_ONCE_MARKER: &str = "UGG_SYNTH_ONCE ";
/// 一発合成の終了コード（`sidecar.py` と揃える）。
const SYNTH_ONCE_OOM: i32 = 2;
const SYNTH_ONCE_NO_GPU: i32 = 3;

/// ゲートの締め切り。**根拠になる実測が無い**（モデルの読み込み時間の記録が docs に 1 件も無い）ので
/// 広めに取り、実機検証で所要時間を記録する（合格したときの行に、読み込みを含めた時間を出す）。
/// 無進捗 5 分は「CPU を使い続けて終わらない読み込み」では鳴らないので、締め切りが別に要る。
const GATE_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// 落ちたときに「VRAM 不足の疑い」とみなす、開始時の空き VRAM（MB）。
///
/// **v0.5.7 項目 6 で根拠を v4.1-Small・bf16 に取り直した**（値は同じ）: 1 プロセスに 1 つだけ読み込み、透かし込みで
/// 16 ステップの合成と 40 ステップの参照音声の生成を通したときの torch の確保量のピークは 3.31 GB
/// （spec §6.0 の spike。v3・fp32 は 3.73 GB）。CUDA の文脈（数百 MB）を足して 4 GB を下回れば、足りなかったと見る。
/// ゲートは合成と生成で同じランタイムを使い回すので、生成を足しても 2 つ分にはならない。
const GATE_VRAM_SUSPECT_MB: u64 = 4096;

/// ゲートの作業場所（参照音声の写しと、事前変換の結果が置かれる。終わったら消す）。
const GATE_DIR: &str = ".update-gate";

/// torch が CUDA を使えるかを聞くときの目印。
const CUDA_PROBE_MARKER: &str = "UGG_CUDA ";

/// 子プロセスがどう終わったか（ゲートの判定に要る分だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateExit {
    Code(Option<i32>),
    /// 締め切り・無進捗で止めた。
    TimedOut,
}

/// 一発合成の結果（v0.5.6 項目 3b）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateOutcome {
    /// 合成できた（モデルの読み込みを含めた時間）。`watermark` は透かしが効いていたか（v0.5.7 項目 5。
    /// 古い `sidecar.py` は返さないので `None`。合否には使わない — 透かしが無くても合成はできる）。
    Passed { ms: u64, watermark: Option<bool>, voice_design: bool },
    /// 参照音声が 1 つも無く、生成も試さない呼び出しだった（戻したあとの確かめ直し。v0.5.7 項目 6 からの
    /// 更新のゲートは生成した声で合成するので、これにならない）。
    NoVoiceRef,
    /// torch から GPU が見えない。
    NoGpu,
    /// GPU のメモリ不足（例外として捕まえられた）。
    OutOfMemory,
    /// 合成できなかった（理由つき）。
    Failed(String),
    /// 結果の行を出さずに終わった（例外にならずプロセスごと落ちた）。開始時の空き VRAM があれば添える。
    Crashed { code: Option<i32>, vram_free_mb: Option<u64> },
    /// 締め切りまでに終わらなかった。
    TimedOut,
}

/// 子プロセスの終わり方と、目印の 2 行から結果を決める（純関数）。
fn classify_gate(
    exit: GateExit,
    start: Option<&serde_json::Value>,
    result: Option<&serde_json::Value>,
) -> GateOutcome {
    if exit == GateExit::TimedOut {
        return GateOutcome::TimedOut;
    }
    let vram_free_mb = start
        .and_then(|s| s.get("vram_free_mb"))
        .and_then(serde_json::Value::as_u64);
    let Some(result) = result else {
        let GateExit::Code(code) = exit else {
            return GateOutcome::TimedOut;
        };
        // 結果の行が無くても、終了コードが意味を持つことがある。
        return match code {
            Some(SYNTH_ONCE_OOM) => GateOutcome::OutOfMemory,
            Some(SYNTH_ONCE_NO_GPU) => GateOutcome::NoGpu,
            _ => GateOutcome::Crashed { code, vram_free_mb },
        };
    };
    let ok = result.get("ok").and_then(serde_json::Value::as_bool) == Some(true);
    let bytes = result.get("bytes").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if ok && bytes > 0 && exit == GateExit::Code(Some(0)) {
        let ms = result.get("ms").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let watermark = result.get("watermark").and_then(serde_json::Value::as_bool);
        // v0.5.7 項目 6: 参照音声の生成も確かめたか（古い sidecar.py は返さない＝確かめていない）
        let voice_design = result
            .get("voice_design")
            .and_then(|v| v.get("ok"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        return GateOutcome::Passed { ms, watermark, voice_design };
    }
    match result.get("kind").and_then(serde_json::Value::as_str) {
        Some("oom") => GateOutcome::OutOfMemory,
        Some("no_gpu") => GateOutcome::NoGpu,
        _ => GateOutcome::Failed(
            result
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(crate::dialogue::llm::truncate_for_log)
                .unwrap_or_else(|| "理由の分からない失敗".to_string()),
        ),
    }
}

/// ゲートの結果をどう扱うか。
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateVerdict {
    /// 合格。
    Pass(String),
    /// 確かめられなかったが、更新は成立させる（ユーザー裁定 2026-09-20: 参照音声が無い・元から GPU が
    /// 見えない環境）。
    Skip(String),
    /// VRAM が足りない。**全部は戻さず保留する**（v0.5.7 項目 6、2026-09-26 ユーザー裁定。v0.5.6 項目 3b が
    /// 「v3 が transformers 5 で動くと分かれば再判断する」とした件 — spike で動くと確かめた）。新しい依存と取得済みの
    /// モデルは残し、記録は依存だけを進め、モデルの欄は旧のまま（声は旧モデルのまま）。次の更新はゲートだけになる。
    HoldForVram(String),
    /// 更新で壊れた疑い。全部戻し、元の状態でもう一度試して切り分ける。
    RollBack(String),
}

/// ゲートの結果の扱いを決める（純関数）。
///
/// **GPU は差分で見る**（反証レビュー #1）。「GPU が見えなければ飛ばす」だけだと、更新が CUDA を壊した
/// ときも成功として記録が進み、更新ボタンが消えて戻す導線ごと失われる。**前は使えたのに使えなく
/// なった**なら更新のせいとして戻す。元から使えない（または分からない）環境だけ飛ばす。
fn gate_verdict(outcome: &GateOutcome, cuda_before: Option<bool>) -> GateVerdict {
    match outcome {
        GateOutcome::Passed { ms, watermark, voice_design } => GateVerdict::Pass(format!(
            "合成できました（モデルの読み込みを含めて {:.1} 秒{}{}）",
            *ms as f64 / 1000.0,
            if *voice_design { "。参照音声の生成も確かめました" } else { "" },
            // v0.5.7 項目 5: 効いていないことがまた見えなくならないように（v0.5.4 は一度も効いていないことに
            // 実機検証で初めて気づいた）
            match watermark {
                Some(true) => "。透かし: 効いています",
                Some(false) => "。透かし: 効いていません",
                None => "",
            }
        )),
        GateOutcome::NoVoiceRef => GateVerdict::Skip(
            "参照音声がまだ無いので、合成は確かめられませんでした（更新は済ませました）".to_string(),
        ),
        GateOutcome::NoGpu if cuda_before == Some(true) => GateVerdict::RollBack(
            "更新のあと、GPU（CUDA）が使えなくなりました".to_string(),
        ),
        GateOutcome::NoGpu => GateVerdict::Skip(
            "GPU が見えないので、合成は確かめられませんでした（更新は済ませました）".to_string(),
        ),
        GateOutcome::OutOfMemory => GateVerdict::HoldForVram(
            "GPU のメモリ（VRAM）が足りず、合成で確かめられませんでした".to_string(),
        ),
        GateOutcome::Crashed {
            vram_free_mb: Some(free),
            ..
        } if *free < GATE_VRAM_SUSPECT_MB => GateVerdict::HoldForVram(format!(
            "合成の確認の途中で Python が終了しました。始めたときの空き VRAM が {free} MB で、足りなかったと見られます"
        )),
        GateOutcome::Crashed { code, .. } => GateVerdict::RollBack(format!(
            "合成の確認の途中で Python が異常終了しました (code {code:?})"
        )),
        GateOutcome::Failed(why) => GateVerdict::RollBack(format!("更新したランタイムで合成できませんでした: {why}")),
        GateOutcome::TimedOut => GateVerdict::RollBack(format!(
            "合成の確認が {} 分で終わりませんでした",
            GATE_DEADLINE.as_secs() / 60
        )),
    }
}

/// 戻したあとの確認の結果を、エラーに添える（純関数。反証レビュー #2）。
///
/// **絶対値で「更新のせい」と決めない。** 合成は更新と関係の無い理由（共有の HF キャッシュが掃除された・
/// 参照音声が壊れている）でも落ちる。戻した状態で合成できれば更新が原因、できなければ元から合成できない
/// 環境（更新のせいではない）と言い分ける。
fn explain_after_recheck(err: anyhow::Error, recheck: &GateOutcome) -> anyhow::Error {
    match recheck {
        GateOutcome::Passed { .. } => anyhow!("{err:#}。元に戻した状態では合成できたので、更新が原因です"),
        GateOutcome::OutOfMemory | GateOutcome::Crashed { .. } | GateOutcome::TimedOut => {
            anyhow!("{err:#}。元に戻した状態でも合成を確かめられませんでした（更新のせいかは分かりません）")
        }
        GateOutcome::Failed(why) => anyhow!(
            "{err:#}。元に戻した状態でも合成できませんでした — 更新の前から、この環境では合成できていません（更新のせいではありません）: {why}"
        ),
        GateOutcome::NoVoiceRef | GateOutcome::NoGpu => err,
    }
}

/// ゲートの材料にする参照音声（ユーザー裁定 2026-09-20: いまある参照音声を使う）。
///
/// `refs\` の wav から、メイン → サブ → その他の順、同じ順位なら新しいものを選ぶ。DB は見ない
/// （更新の経路は DB に触れない。記録の無い wav も、合成の材料としては同じく使える）。
fn pick_gate_voice_ref(asset_root: &Path) -> Option<PathBuf> {
    let refs = asset_root.join("refs");
    let entries = std::fs::read_dir(&refs).ok()?;
    let mut candidates: Vec<(u8, std::cmp::Reverse<std::time::SystemTime>, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            if !name.to_ascii_lowercase().ends_with(".wav") || !path.is_file() {
                return None;
            }
            let rank = if name.starts_with("main_") {
                0
            } else if name.starts_with("sub_") {
                1
            } else {
                2
            };
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((rank, std::cmp::Reverse(modified), path))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next().map(|(_, _, path)| path)
}

/// 1 回だけ合成して確かめる（v0.5.6 項目 3b）。`model_args` は試すモデル（取得したいまのビルドの値か、
/// 戻したあとの読み先）。**HTTP は使わない**（更新中は錠で塞がっている）ので、子プロセスとして走らせる。
/// ゲートの子プロセスに渡す引数（純関数。v0.5.7 項目 6）。生成も試すなら作業場所（`--gate-dir`）を渡し、
/// 参照音声の写しがあれば `--voice-ref` も渡す（無ければ生成した声で合成する）。
fn gate_args(
    asset_root: &Path,
    work: &Path,
    voice_copy: Option<&Path>,
    with_voice_design: bool,
    model_args: &[String],
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        asset_root.join("sidecar.py").into(),
        "--asset-dir".into(),
        asset_root.into(),
        "--synth-once".into(),
    ];
    if let Some(copy) = voice_copy {
        args.push("--voice-ref".into());
        args.push(copy.into());
    }
    if with_voice_design {
        args.push("--gate-dir".into());
        args.push(work.into());
    }
    args.extend(model_args.iter().map(std::ffi::OsString::from));
    args
}

/// 1 回合成して確かめる（v0.5.6 項目 3b）。**`with_voice_design` なら参照音声の生成も 1 回試す**（v0.5.7 項目 6。
/// 乗り換えでは生成も新しいモデルにまとめ、成功したら旧モデルを消すので、生成を確かめずに通さない）。
/// 参照音声が無くても、生成した声で合成すれば両方を確かめられる。**戻したあとの確かめ直しは合成だけ**
/// （旧モデルでは合成と生成が別のモデルで、生成まで試すと VRAM を余計に使い、確かめ直しそのものが落ちうる）。
fn run_synth_gate<F>(
    asset_root: &Path,
    py_exe: &Path,
    model_args: &[String],
    with_voice_design: bool,
    mut on_line: F,
) -> GateOutcome
where
    F: FnMut(&str),
{
    let voice = pick_gate_voice_ref(asset_root);
    if voice.is_none() && !with_voice_design {
        return GateOutcome::NoVoiceRef;
    }
    // 参照音声は**写しを渡す**。事前変換の結果は参照 wav の隣に作られるので、元の場所で走らせると
    // ユーザーの refs に試験の変換結果が残る。他のプロセスが掴んでいるファイルで落ちることも避けられる。
    // 生成した声も同じ作業場所に置き、終わったら消す。
    let work = asset_root.join(GATE_DIR);
    let _ = std::fs::remove_dir_all(&work);
    if let Err(err) = std::fs::create_dir_all(&work) {
        return GateOutcome::Failed(format!("作業場所を作れません: {err}"));
    }
    let copy = work.join("ref.wav");
    if let Some(voice) = &voice {
        if let Err(err) = std::fs::copy(voice, &copy) {
            let _ = std::fs::remove_dir_all(&work);
            return GateOutcome::Failed(format!("参照音声を作業場所へ写せません: {err}"));
        }
    }
    on_line(if with_voice_design {
        "更新したランタイムで、参照音声の生成と合成を 1 回ずつ試して確かめています…（モデルの読み込みに時間がかかります）"
    } else {
        "1 回合成して確かめています…（モデルの読み込みに時間がかかります）"
    });
    let mut cmd = Command::new(py_exe);
    cmd.args(gate_args(
        asset_root,
        &work,
        voice.as_ref().map(|_| copy.as_path()),
        with_voice_design,
        model_args,
    ));
    let mut start: Option<serde_json::Value> = None;
    let mut result: Option<serde_json::Value> = None;
    let ended = child_process::off_the_async_workers(|| {
        child_process::run_streaming_until(cmd, None, Some(PYTHON_STALL_AFTER), Some(GATE_DEADLINE), |l| {
            // 目印は START を先に見る（`UGG_SYNTH_ONCE` は START の頭と同じ）。
            if let Some(json) = l.text.strip_prefix(SYNTH_ONCE_START_MARKER.trim_end()) {
                start = serde_json::from_str(json.trim()).ok();
            } else if let Some(json) = l.text.strip_prefix(SYNTH_ONCE_MARKER.trim_end()) {
                result = serde_json::from_str(json.trim()).ok();
            } else if !l.overwritten {
                on_line(l.text);
            }
        })
    });
    let _ = std::fs::remove_dir_all(&work);
    let exit = match ended {
        Ok(Ended::Exited(status)) => GateExit::Code(status.code()),
        Ok(Ended::Stalled | Ended::TimedOut) => GateExit::TimedOut,
        Err(err) => return GateOutcome::Failed(format!("python 起動失敗: {err}")),
    };
    classify_gate(exit, start.as_ref(), result.as_ref())
}

/// torch から CUDA が使えるか（更新の前に聞いておく。分からなければ `None`）。
fn probe_cuda(py_exe: &Path) -> Option<bool> {
    let script = format!(
        "import json,torch\nprint({marker:?}+json.dumps(bool(torch.cuda.is_available())))",
        marker = CUDA_PROBE_MARKER,
    );
    let mut found = None;
    let _ = run_python_lines(py_exe, &["-c", &script], false, |l| {
        if let Some(json) = l.text.strip_prefix(CUDA_PROBE_MARKER.trim_end()) {
            found = serde_json::from_str::<bool>(json.trim()).ok();
        }
    });
    found
}

const GIB: u64 = 1024 * 1024 * 1024;

/// **取得量の目安の正本**（v0.5.7 項目 8）。確認の文言（`settings.ts`）と取説（`manual.md`）は同じ文字列を書き、
/// 契約テストが突き合わせる（以前は 3 か所で「約 2〜3 GB」「約 2〜4 GB」「合計 10 GB 前後」と食い違っていた）。
/// 初回導入の取得（PyTorch 2.87 GB・v4.1-Small 3.07 GB・コーデック 0.43 GB・ほかの依存 約 0.25 GB・透かし 0.07 GB）。
/// この 3 つは Rust の文言には使わず、画面と取説の正本として契約テストだけが読む（テストの外では未使用になる）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const FIRST_INSTALL_DOWNLOAD: &str = "約 7 GB";
/// 初回導入のあとに使うディスク（Python と依存 約 5.4 GB・モデル 約 3.5 GB）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const FIRST_INSTALL_DISK: &str = "約 9 GB";
/// AI モデルが変わる更新の取得（v4.1-Small 3.07 GB・コーデック 0.43 GB・透かし 0.07 GB・依存の差分）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MODEL_UPDATE_DOWNLOAD: &str = "約 3.6 GB";
/// AI モデル本体の取得（v4.1-Small とコーデック）。
pub(crate) const MODEL_DOWNLOAD: &str = "約 3.5 GB";
/// PyTorch（CUDA 12.8）の取得（torch 2.10.0+cu128 の wheel が 2.87 GB。2026-09-27 に配布元へ大きさを問い合わせた）。
pub(crate) const TORCH_DOWNLOAD: &str = "約 2.9 GB";

/// **初回導入に要るディスクの空き**（v0.5.7 項目 8）。取得は約 7 GB（PyTorch 2.87 GB・v4.1-Small 3.07 GB・
/// コーデック 0.43 GB・ほか）、入れた後は約 9 GB、pip の一時ファイルの分を足して余裕を持たせる。
pub(crate) const REQUIRED_FREE_FIRST_INSTALL: u64 = 11 * GIB;

/// **更新に要るディスクの空き**（v0.5.7 項目 8、純関数）。更新が成功するまで旧モデルも残るが、既にディスクに
/// あるので足さない。モデルが変わるなら約 3.6 GB の取得、PyTorch が変わるなら約 3 GB の取得に余裕を足す。
fn required_free_for_update(plan: &UpdatePlan) -> u64 {
    let mut need = GIB;
    if !plan.torch.is_empty() {
        need += 4 * GIB;
    }
    if plan.models {
        need += 5 * GIB;
    }
    need
}

/// 空きが足りなければ理由（純関数）。空きが分からなければ止めない（分からないことを理由に断らない）。
fn lacking_free_space(required: u64, free: Option<u64>) -> Option<String> {
    let free = free?;
    if free >= required {
        return None;
    }
    let gb = |b: u64| b as f64 / GIB as f64;
    Some(format!(
        "ディスクの空きが足りません（要る量の目安 {:.1} GB、いまの空き {:.1} GB）。あと {:.1} GB ほど空けてから、もう一度お試しください（何も変えていません）",
        gb(required),
        gb(free),
        gb(required - free)
    ))
}

/// `path` のあるドライブの空き容量（バイト）。分からなければ `None`。
fn free_bytes(path: &Path) -> Option<u64> {
    let mut at = path;
    while !at.exists() {
        at = at.parent()?;
    }
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let mut free: u64 = 0;
        let wide = windows::core::HSTRING::from(at.as_os_str());
        // SAFETY: 出力先はこの関数の中の u64。ほかの 2 つは受け取らない。
        unsafe { GetDiskFreeSpaceExW(&wide, Some(&mut free as *mut u64), None, None) }.ok()?;
        Some(free)
    }
    #[cfg(not(windows))]
    {
        let _ = at;
        None
    }
}

/// 初回導入の前に空きを確かめる（v0.5.7 項目 8）。足りなければ始めずに、要る量を伝える。
pub fn check_free_space_for_first_install(asset_root: &Path) -> Result<()> {
    match lacking_free_space(REQUIRED_FREE_FIRST_INSTALL, free_bytes(asset_root)) {
        Some(why) => Err(anyhow!(why)),
        None => Ok(()),
    }
}

/// VRAM 不足で保留したときに、記録を進めてよい名前（v0.5.7 項目 6）。**モデルは進めない** — 記録が読み先の正本
/// （v0.5.6 項目 3a）なので、進めると確かめていない新しいモデルを読みに行く。パッケージ（名前付き要件と固定 URL）は
/// 入れ替え済みなので進める（進めないと、次の更新が入れ直しをまた最初からやる）。
fn names_committed_on_hold(plan: &UpdatePlan) -> Vec<String> {
    let models = current_models();
    plan.names.iter().filter(|n| !models.contains_key(*n)).cloned().collect()
}

/// 記録の値（`repo@revision`）からモデルの置き場所の名前を作る（`model_args_from` と同じ切り方）。
fn model_dir_of(value: &str) -> String {
    match value.rsplit_once('@') {
        Some((repo, rev)) if !repo.is_empty() && !rev.is_empty() => model_dir_name(repo, rev),
        _ => model_dir_name(value, "main"),
    }
}

/// 更新で使われなくなったモデルの置き場所（純関数、v0.5.7 項目 10）。更新の前に読んでいた先（`before`）のうち、
/// いまの読み先（`now`）の置き場所に入らないもの。v3 から v4.1 への更新なら、v3・v2-VoiceDesign・revision を固定する
/// 前のコーデックの写しの 3 つ。**`before` に無いものは挙げない**（`model\` にあっても ugg が読んでいた証拠が無い）。
fn old_model_dirs(
    before: &std::collections::BTreeMap<String, String>,
    now: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let keep: std::collections::BTreeSet<String> = now.values().map(|v| model_dir_of(v)).collect();
    let old: std::collections::BTreeSet<String> = before
        .values()
        .map(|v| model_dir_of(v))
        .filter(|d| !keep.contains(d))
        .collect();
    old.into_iter().collect()
}

/// 旧モデルの片付けの結果。
#[derive(Debug, Default, PartialEq)]
struct OldModelCleanup {
    /// 消したモデルの置き場所の名前。
    removed_models: Vec<String>,
    /// 消した参照音声の変換結果の数。
    removed_latents: usize,
    /// 消せなかったもの（パスと理由）。
    failed: Vec<String>,
}

/// **旧モデルを片付ける**（v0.5.7 項目 10、spec §6.0）。**ゲートで合成と生成の両方が通って記録を書いたあとにだけ呼ぶ**
/// （確かめられなかったとき・保留したときは呼ばない。旧モデルは全戻しと v3 の材料）。
///
/// - `model\` のうち、更新の前に読んでいて、いまの読み先に入らない置き場所（約 4 GB）
/// - 参照音声の隣の変換結果（`<stem>.<合成>+<コーデック>.<精度>.<前処理>.latent.pt`、書きかけの `.tmp` を含む）のうち、
///   いまの読み先のモデルのものでないもの（`sidecar.py` の `ref_latent_path` と同じ名前の形）
///
/// **共有 HF キャッシュには触れない**（v3 が合成時に取ったトークナイザなどは据え置く）。パッケージも消さない。
/// 更新そのものは済んでいるので、消せなくても止めずに理由を返す。
fn clean_up_old_models(
    asset_root: &Path,
    before: &std::collections::BTreeMap<String, String>,
    now: &std::collections::BTreeMap<String, String>,
) -> OldModelCleanup {
    let mut out = OldModelCleanup::default();
    let model_root = asset_root.join("model");
    for name in old_model_dirs(before, now) {
        let dir = model_root.join(&name);
        if !dir.exists() {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => out.removed_models.push(name),
            Err(e) => out.failed.push(format!("{}（{e}）", dir.display())),
        }
    }
    // 変換結果は、いまの合成とコーデックの組のものだけ残す。組が分からなければ触らない。
    let (Some(synth), Some(codec)) = (now.get("model_synth"), now.get("model_codec")) else {
        return out;
    };
    let current = format!(".{}+{}.", model_dir_of(synth), model_dir_of(codec));
    let suffix = crate::tts::voice_ref::REF_LATENT_SUFFIX;
    let tmp_suffix = format!("{suffix}.tmp");
    let Ok(entries) = std::fs::read_dir(asset_root.join("refs")) else {
        return out;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        let is_latent = file.ends_with(suffix) || file.ends_with(&tmp_suffix);
        if !is_latent || file.contains(&current) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => out.removed_latents += 1,
            Err(e) => out.failed.push(format!("{}（{e}）", entry.path().display())),
        }
    }
    out
}

/// 片付けの結果を画面とログに出す。
fn report_old_model_cleanup(cleanup: &OldModelCleanup, mut on_line: impl FnMut(&str)) {
    if !cleanup.removed_models.is_empty() || cleanup.removed_latents > 0 {
        let message = format!(
            "古いモデルを片付けました（{}。参照音声の変換結果 {} 件）",
            if cleanup.removed_models.is_empty() {
                "モデルは無し".to_string()
            } else {
                cleanup.removed_models.join(" / ")
            },
            cleanup.removed_latents
        );
        crate::ulog!("[irodori] {message}");
        on_line(&message);
    }
    if !cleanup.failed.is_empty() {
        let message = format!(
            "古いモデルのうち消せなかったものがあります（使っていないので、手で消して構いません）: {}",
            cleanup.failed.join(" / ")
        );
        crate::ulog!("[irodori] {message}");
        on_line(&message);
    }
}

/// 古くなった分だけを入れ直す (v0.5.4 項目 3 / v0.5.6 項目 3c・3d、spec §6.0)。
///
/// **1 つのトランザクションにする。** 途中のどこで失敗しても、入れ替えたものを**全部**戻す
/// （以前は、後の段で失敗すると、それより前に成功した分の退避＝唯一の旧版を戻さずに消していた。
/// 名前付き要件には退避も復元も無かった）。記録（`installed.json`）は全部成功したときにだけ書く。
///
/// 1. 前回の更新が途中で止まっていれば、先に元へ戻す（`recover_interrupted_update`）
/// 2. 入っている全配布の版を控える（控えを取れなければ何も変えずに止まる）
/// 3. 計画の順に入れ直す（`update_plan` / `apply_update_plan`）
/// 4. **1 回合成して確かめる**（`run_synth_gate` / `gate_verdict`。v0.5.6 項目 3b）
/// 5. 失敗・不合格なら全部戻す（`roll_back_update`）。成功したら退避と控えを捨てて記録する
///
/// 戻り値は「入れ直せた名前」。呼び出し側はこれで記録を部分的に更新する。
pub async fn update_irodori_runtime<F>(
    asset_root: &Path,
    outdated: &[String],
    mut on_line: F,
) -> Result<Vec<String>>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!(
            "Python ランタイムがありません。先に初回導入を行ってください: {}",
            py_exe.display()
        ));
    }

    if outdated.iter().any(|n| n == "python") {
        // ここだけは安全に入れ直せない。黙って部分更新して「最新」と記録するより、
        // 何が要るかを伝えて止まるほうがよい。
        return Err(anyhow!(
            "Python 本体の版が変わっています。この経路では入れ直せません（稼働中のインタプリタを差し替えられないため）。`%APPDATA%\\ugg\\irodori\\python` を削除してから、もう一度導入してください"
        ));
    }

    recover_interrupted_update(asset_root, &py_exe, |l| on_line(l))?;
    // 更新の前に読んでいたモデル（v0.5.7 項目 10。成功したら、ここに入っていて使われなくなったものを片付ける）
    let models_before = models_to_read(read_stamp(asset_root).as_ref());

    let plan = update_plan(outdated);
    for name in &plan.skipped {
        on_line(&format!("{name} は入れ直しの対象外です (skip)"));
    }
    // **空きを確かめてから始める**（v0.5.7 項目 8）。成功するまで旧モデルと新しいモデルが同時に残る。
    if let Some(why) = lacking_free_space(required_free_for_update(&plan), free_bytes(asset_root)) {
        on_line(&why);
        return Err(anyhow!(why));
    }

    // 入れ直す前に「いま何が使えるか」を控える。ここを控えずに絶対値で判定すると、
    // 元から import できないものを理由に、正常な入れ直しまで巻き戻してしまう。
    let before_imports = import_report(&py_exe);
    for (m, err) in &before_imports {
        if let Some(why) = err {
            on_line(&format!("注意: {m} は入れ直す前から import できません ({why})"));
        }
    }

    // **GPU が使えるかを先に聞いておく**（v0.5.6 項目 3b。ゲートで GPU が見えなかったとき、
    // 「元から見えない環境」と「更新で壊れた」を言い分けるため）。パッケージを入れ替えないなら
    // CUDA は壊れようがないので聞かない（torch の読み込みに数秒かかる）。
    let cuda_before = if plan.touches_packages() {
        probe_cuda(&py_exe)
    } else {
        None
    };

    // **版の控え**（v0.5.6 項目 3d）。パッケージを入れ替えるときだけ取る（モデルだけの更新・
    // 前回の後始末だけの呼び出しでは、戻す対象が無い）。取れなければ、戻す手段が無いので何も変えずに止まる。
    let touches_packages = plan.touches_packages();
    let before_versions = if touches_packages {
        let versions = query_all_versions(&py_exe, |l| on_line(l))
            .context("更新の前に、いま入っている版を控えられませんでした（何も変えていません）")?;
        write_versions_snapshot(asset_root, &versions)
            .context("更新の前に、いま入っている版の控えを書けませんでした（何も変えていません）")?;
        versions
    } else {
        Default::default()
    };

    let applied = apply_update_plan(
        asset_root,
        &py_exe,
        &plan,
        &before_imports,
        &before_versions,
        |l| on_line(l),
    )
    .await;
    let versions = touches_packages.then_some(&before_versions);
    if let Err(err) = applied {
        return Err(roll_back_update(asset_root, &py_exe, versions, err, |l| on_line(l)));
    }

    // ④ **1 回合成して確かめる**（v0.5.6 項目 3b）。試すのは**取得したいまのビルドの値**（旧い読み先で
    // 試しても「コードだけ新しくて重みが無い」を捕まえられない）。
    // 入れ直すものが無い呼び出し（前回の後始末だけ）では確かめない（何も変えていない）。
    let outcome = if plan.names.is_empty() {
        GateOutcome::Passed { ms: 0, watermark: None, voice_design: false }
    } else {
        run_synth_gate(asset_root, &py_exe, &model_args_for_fetch(), true, |l| on_line(l))
    };
    match gate_verdict(&outcome, cuda_before) {
        GateVerdict::Pass(_) if plan.names.is_empty() => {}
        GateVerdict::Pass(message) => {
            // 読み込みを含めた所要時間の記録（締め切りの根拠になる実測が無いため、実機で集める）。
            crate::ulog!("[irodori] 更新の確認: {message}");
            on_line(&message);
        }
        GateVerdict::Skip(message) => {
            crate::ulog!("[irodori] 更新の確認: {message}");
            on_line(&message);
        }
        GateVerdict::HoldForVram(why) => {
            // **全部は戻さず保留する**（v0.5.7 項目 6、ユーザー裁定）。新しい依存の上の旧モデルは spike で
            // 確かめた組み合わせ（spec §6.0 の 1.）。数 GB を消しては取り直す往復をしない。
            let _ = std::fs::remove_dir_all(asset_root.join(UPDATE_BACKUP_DIR));
            remove_versions_snapshot(asset_root);
            let held = names_committed_on_hold(&plan);
            record_after_install(asset_root, &held, |l| on_line(l))
                .context("パッケージの入れ替えは済みましたが、導入記録を書けませんでした")?;
            let message = format!(
                "{why}。パッケージの入れ替えは済ませ、声はいままでのモデルのままにしています（保留）。\
                 GPU を使うほかのアプリを止めて VRAM を空けてから、もう一度「更新する」を押してください\
                 （モデルは取得済みなので、取り直さずに確かめだけを行います）"
            );
            crate::ulog!("[irodori] 更新を保留しました: {message}");
            on_line(&message);
            return Err(anyhow!(message));
        }
        GateVerdict::RollBack(why) => {
            let err = roll_back_update(asset_root, &py_exe, versions, anyhow!(why), |l| on_line(l));
            // **絶対値で「更新のせい」と決めない**（反証レビュー #2）。戻した状態でもう一度試して切り分ける。
            on_line("元に戻した状態でも合成できるかを確かめています…（更新のせいかを切り分けます）");
            let (read_args, _) = model_args_for_read(asset_root);
            let recheck = run_synth_gate(asset_root, &py_exe, &read_args, false, |l| on_line(l));
            return Err(explain_after_recheck(err, &recheck));
        }
    }

    // ここまで来たら全部成功している。退避と控えを捨てる。
    let _ = std::fs::remove_dir_all(asset_root.join(UPDATE_BACKUP_DIR));
    remove_versions_snapshot(asset_root);

    // **記録は更新の一部。** これをコマンド層に置いていたためテストから到達できず、
    // 「入れ直したのに `up_to_date` が false のまま」を自動で検出できなかった。
    record_after_install(asset_root, &plan.names, |l| on_line(l))
        .context("入れ直しは成功しましたが、導入記録を書けませんでした")?;

    // ⑤ **旧モデルを片付ける**（v0.5.7 項目 10）。**合成と生成の両方を確かめ、記録を書いたあとだけ。**
    // 確かめられなかったとき（GPU が無い等）はここに来ても消さない。保留は上で返っている。
    if matches!(outcome, GateOutcome::Passed { voice_design: true, .. }) {
        let now = models_to_read(read_stamp(asset_root).as_ref());
        report_old_model_cleanup(&clean_up_old_models(asset_root, &models_before, &now), |l| on_line(l));
    }

    Ok(plan.names)
}

/// 透かし（SilentCipher）の重みを、上流が読みに行く共有 HF キャッシュへ先に取る（v0.5.7 項目 5、spec §6.0）。
///
/// 取らないと、pydub が入った環境の**最初の合成で上流が黙って取りに行く**（約 68 MB）。更新の途中で全戻し
/// しても pydub などの新しく入った配布は残る（消すほうが別の依存を壊しうる）ので、**更新の最初の段**で取る。
/// **失敗しても導入・更新は止めない**（透かしが無くても合成はできる）。呼び出し側が理由を画面とログに出す。
pub fn prefetch_watermark_weights<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    let sidecar_py = asset_root.join("sidecar.py");
    if !sidecar_py.is_file() {
        return Err(anyhow!("sidecar.py が配置されていません: {}", sidecar_py.display()));
    }
    on_line("透かし（SilentCipher）の重みを取得しています…（約 68 MB）");
    let asset_root_str = asset_root.to_string_lossy().into_owned();
    let sidecar_py_str = sidecar_py.to_string_lossy().into_owned();
    run_python(
        &py_exe,
        &[sidecar_py_str.as_str(), "--asset-dir", asset_root_str.as_str(), "--fetch-watermark"],
        |l| on_line(l),
    )
}

/// 透かしの重みの先取りに失敗したときに、止めずに理由を伝える（初回導入と更新で共通）。
pub fn report_watermark_prefetch_failure<F>(err: &anyhow::Error, mut on_line: F)
where
    F: FnMut(&str),
{
    let msg = format!(
        "透かしの重みを取得できませんでした（続けます。透かしは合成のときにもう一度取りに行きます）: {err:#}"
    );
    crate::ulog!("[irodori] {msg}");
    on_line(&msg);
}

/// 6) HF モデル本体を sidecar.py の `--download-only` モードで取得する (M4c Phase G)。
///
/// 通常のサイドカー起動経路 (`--no-download`) では DL を skip するように切り替えたため、
/// マルチGBのモデル取得は本ステップで完了させておく。`--download-only` モードは uvicorn を
/// 起動せず、download_models 完了で即終了する。stderr の `[hf-download] ...` 行は run_python
/// が on_line に流すので、`download_irodori_assets` の `irodori-download` event に伝わる。
pub async fn install_irodori_models<F>(
    asset_root: &Path,
    sidecar_py: &Path,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!(
            "python.exe が見つかりません: {}",
            py_exe.display()
        ));
    }
    if !sidecar_py.is_file() {
        return Err(anyhow!(
            "sidecar.py が配置されていません: {}",
            sidecar_py.display()
        ));
    }
    on_line(&format!("Irodori-TTS の AI モデル（{MODEL_DOWNLOAD}）を確認して、要るものを取得しています…"));
    let asset_root_str = asset_root.to_string_lossy().into_owned();
    let sidecar_py_str = sidecar_py.to_string_lossy().into_owned();
    let models = model_args_for_fetch();
    let mut args: Vec<&str> = vec![
        sidecar_py_str.as_str(),
        "--asset-dir",
        asset_root_str.as_str(),
        "--download-only",
    ];
    // **取得はいまのビルドの値**（読み先は記録から決める。v0.5.6 項目 3a）。渡さないと `sidecar.py` の
    // 既定値で取りに行き、Rust が求めているものと食い違う。
    args.extend(models.iter().map(String::as_str));
    run_python(&py_exe, &args, |l| on_line(l))?;
    Ok(())
}

// ============ 内部ユーティリティ ============

/// `python311._pth` の `#import site` を `import site` に書き換える。
/// Embeddable Python は既定で site-packages を無効化しているのでこの編集が必須。
fn enable_site_packages(py_dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(py_dir)
        .with_context(|| format!("read python dir: {}", py_dir.display()))?;
    for e in entries.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with("._pth") {
            continue;
        }
        let content = std::fs::read_to_string(&p)
            .with_context(|| format!("read {}", p.display()))?;
        let patched = patch_pth(&content);
        std::fs::write(&p, patched)
            .with_context(|| format!("write {}", p.display()))?;
        return Ok(());
    }
    Err(anyhow!("python._pth が {} に見つかりません", py_dir.display()))
}

/// `_pth` の中身を「import site が有効」になるよう書き換える純粋関数 (テスト対象)。
fn patch_pth(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    let mut site_seen = false;
    for line in input.lines() {
        let trimmed = line.trim_start();
        if trimmed == "#import site" || trimmed == "# import site" {
            out.push_str("import site");
            site_seen = true;
        } else if trimmed == "import site" {
            out.push_str(line);
            site_seen = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !site_seen {
        out.push_str("import site\n");
    }
    out
}

/// HTTP GET でファイルに保存。リトライなしの単純実装 (Phase G で必要なら強化)。
async fn download_to(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent: {}", parent.display()))?;
    }
    // 上限を付けたクライアントを使う（v0.5.6 項目 2）。**この経路は導入の錠を握ったまま待つ**ので、
    // 相手が生きたまま黙ると、再起動するまで導入も更新も押せない。
    let resp = crate::tts::download::http_client()
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status {url}"))?;
    let bytes = resp.bytes().await.with_context(|| format!("read body {url}"))?;
    let mut f = std::fs::File::create(dest)
        .with_context(|| format!("create {}", dest.display()))?;
    f.write_all(&bytes)
        .with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

/// 子プロセスにコンソール窓を出させない起動のフラグ。
///
/// **これが無いと、pip や Expand-Archive を呼ぶたびに黒いコンソール窓が前面に出る。**
/// v0.5.4 で「更新する」を押したときに実機で確認した。リリース版は
/// `windows_subsystem = "windows"` でコンソールを持たないため、子プロセスが
/// 自前で窓を割り当ててしまう。stdout/stderr のパイプはこのフラグでは変わらない。
/// （`notepad.exe` で取説を開く経路は、窓が出るのが目的なので対象外）
///
/// 出力を読む子プロセスには `child_process::run_streaming` が付ける。ここで直に使うのは
/// zip の展開（出力を読まない）だけ。
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// PowerShell の単引用符文字列へ埋め込める形にする (v0.5.3)。
///
/// 単引用符の中では `'` を `''` と二重にするのが唯一のエスケープ。素通しすると
/// **`O'Neil` のようにアポストロフィを含むユーザー名のパスで引用が壊れ、導入が失敗する**
/// (Codex レビュー 2026-09-06)。パスはアプリ側が決めるため実害は限定的。
fn ps_single_quoted(p: &Path) -> String {
    p.display().to_string().replace("'", "''")
}

/// Windows PowerShell の `Expand-Archive` で zip を展開。追加 crate なし。
fn expand_zip_windows(zip: &Path, dest: &Path) -> Result<()> {
    let cmd = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        ps_single_quoted(zip),
        ps_single_quoted(dest)
    );
    // 呼び出し元は非同期（`ensure_python_embeddable`）。待つ間ワーカーを塞がない（v0.5.6 項目 2）。
    let status = child_process::off_the_async_workers(|| {
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        // **ugg と一緒に終わらせる**（v0.5.6 項目 4）。展開の途中で ugg が落ちても、展開だけが裏で
        // 続いて導入先を書き換え続けることはない。入れられなくても展開は続ける。
        if let Err(err) = child_process::tie_to_ugg(child.as_raw_handle()) {
            crate::ulog!("[irodori] zip の展開を ugg と一緒に終わらせる設定にできません: {err}");
        }
        child.wait()
    })
    .with_context(|| "Expand-Archive 起動失敗")?;
    if !status.success() {
        return Err(anyhow!(
            "Expand-Archive 異常終了 (code {:?})",
            status.code()
        ));
    }
    Ok(())
}

/// 無進捗とみなすまでの時間（v0.5.6 項目 2）。
///
/// 数えるのは「出力も、読み書きも、CPU も無い」時間（`child_process`）。pip が黙って torch を
/// 展開している間や、パイプ越しで進捗を出さない取得の間は数えないので、本当に止まったときだけ効く。
/// 通信の待ちは pip も huggingface_hub も十数秒で打ち切って再試行するので、5 分は十分に長い。
const PYTHON_STALL_AFTER: Duration = Duration::from_secs(5 * 60);

/// 失敗したときにログへ残す、直前の出力の行数。
const FAILURE_TAIL_LINES: usize = 20;

/// Python を 1 回起動して、出力を行ごとに `on_line` へ流す（stdout と stderr は届いた順に混ざる）。
///
/// 終了コード != 0 で Err。**理由を添える**（pip の `ERROR:` の行・例外の行。以前は
/// 「python 異常終了 (code Some(1))」だけで、pip の「アクセスが拒否されました」は画面の最新の
/// 1 行として一瞬出て消えていた）。直前の出力はログに残す。無進捗が続けば止めて Err。
fn run_python<F>(python_exe: &Path, args: &[&str], mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    run_python_lines(python_exe, args, true, |l| on_line(l.text))
}

/// `run_python` の本体。行がどちらの出力から来たか、上書き（`\r`）かも渡す。
/// `log_failure` が偽なら、失敗してもログに残さない（失敗が想定内の問い合わせ用）。
fn run_python_lines<F>(
    python_exe: &Path,
    args: &[&str],
    log_failure: bool,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(Line<'_>),
{
    let mut cmd = Command::new(python_exe);
    cmd.args(args);
    let mut progress = PipProgress::default();
    let mut hf = HfProgress::default();
    let mut tail = OutputTail::default();
    // 呼び出し元の多くは非同期の関数。待つ間ワーカーを塞がない（v0.5.4 で見送った件）。
    let ended = child_process::off_the_async_workers(|| {
        child_process::run_streaming(cmd, None, Some(PYTHON_STALL_AFTER), |line| {
            if let Some(shown) = progress.describe(line.text).or_else(|| hf.describe(line.text)) {
                if let Some(text) = shown {
                    on_line(Line {
                        text: &text,
                        overwritten: true,
                        ..line
                    });
                }
                return;
            }
            tail.push(line);
            on_line(line);
        })
    })
    .with_context(|| format!("python 起動失敗: {}", python_exe.display()))?;
    let err = match ended {
        Ended::Exited(status) if status.success() => return Ok(()),
        Ended::Exited(status) => match tail.reason() {
            Some(why) => anyhow!("python 異常終了 (code {:?}): {why}", status.code()),
            None => anyhow!("python 異常終了 (code {:?})", status.code()),
        },
        Ended::Stalled => anyhow!(
            "Python の処理が {} 分間、出力も読み書きもしないまま止まっていたので中断しました",
            PYTHON_STALL_AFTER.as_secs() / 60
        ),
        // 締め切りは付けていない（`run_streaming` は無進捗でだけ止める）ので来ないが、来たら同じく中断。
        Ended::TimedOut => anyhow!("Python の処理が時間内に終わらなかったので中断しました"),
    };
    if log_failure {
        crate::ulog!("[irodori:python] {} — {err}", describe_args(args));
        for line in &tail.lines {
            crate::ulog!("[irodori:python]   {line}");
        }
    }
    Err(err)
}

/// ログに載せる引数（`-c` のスクリプトは長く、理由の判断に要らないので省く）。
fn describe_args(args: &[&str]) -> String {
    let shown: Vec<&str> = args
        .iter()
        .map(|a| if a.contains('\n') { "<script>" } else { a })
        .collect();
    crate::dialogue::llm::truncate_for_log(&shown.join(" "))
}

/// 失敗の理由と、ログに残す直前の出力。
#[derive(Default)]
struct OutputTail {
    lines: std::collections::VecDeque<String>,
    /// stderr の行のうち、失敗を述べている最後のもの。
    failure: Option<String>,
    /// stderr の最後の行（pip の新版の告知は除く）。
    last_stderr: Option<String>,
}

impl OutputTail {
    fn push(&mut self, line: Line<'_>) {
        // 進捗の上書き（tqdm）は残さない。残すと直前の出力が進捗だけで埋まる。
        if line.overwritten {
            return;
        }
        if self.lines.len() == FAILURE_TAIL_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line.text.to_string());
        if line.stream == Stream::Stderr {
            if is_failure_line(line.text) {
                self.failure = Some(line.text.to_string());
            }
            if !line.text.starts_with("[notice]") {
                self.last_stderr = Some(line.text.to_string());
            }
        }
    }

    /// 失敗の理由。失敗を述べている行が無ければ、stderr の最後の行。
    fn reason(&self) -> Option<&str> {
        self.failure.as_deref().or(self.last_stderr.as_deref())
    }
}

/// 失敗を述べている行か。pip は `ERROR:` の行のあとに「Check the permissions.」のような
/// 補足や新版の告知を続けるので、最後の行が理由とは限らない。
fn is_failure_line(text: &str) -> bool {
    if text.starts_with("ERROR:") {
        return true;
    }
    if text.starts_with("[hf-download]") {
        return text.contains("失敗");
    }
    // Python の例外の最終行（`ModuleNotFoundError: No module named 'x'` /
    // `huggingface_hub.errors.RepositoryNotFoundError: ...`）
    let head = text.split(':').next().unwrap_or("");
    !head.is_empty()
        && !head.contains(' ')
        && (head.ends_with("Error") || head.ends_with("Exception"))
}

/// pip の `--progress-bar raw` の行（`Progress 1234 of 5678`）を、画面向けの文言に直す。
///
/// raw は 1 秒に 4 行まで出す。画面は最新の 1 行しか見せないので、割合が変わったときだけ出す。
#[derive(Default)]
struct PipProgress {
    shown_percent: Option<u64>,
    /// 大きさが分からない取得で、最後に出したときの量。
    shown_bytes: Option<u64>,
}

impl PipProgress {
    /// pip の進捗の行なら `Some`（中身は出す文言。今回は出さないなら `None`）。進捗でなければ `None`。
    fn describe(&mut self, line: &str) -> Option<Option<String>> {
        let rest = line.strip_prefix("Progress ")?;
        let (done, total) = rest.split_once(" of ")?;
        let done: u64 = done.trim().parse().ok()?;
        let total: u64 = total.trim().parse().ok()?;
        const MB: f64 = 1024.0 * 1024.0;
        if total > 0 {
            let percent = done.saturating_mul(100) / total;
            if self.shown_percent == Some(percent) {
                return Some(None);
            }
            self.shown_percent = Some(percent);
            Some(Some(format!(
                "  取得中 {:.0} / {:.0} MB（{percent}%）",
                done as f64 / MB,
                total as f64 / MB
            )))
        } else {
            const STEP: u64 = 10 * 1024 * 1024;
            let show = match self.shown_bytes {
                None => true,
                Some(prev) => done < prev || done >= prev + STEP,
            };
            if !show {
                return Some(None);
            }
            self.shown_bytes = Some(done);
            Some(Some(format!("  取得中 {:.0} MB", done as f64 / MB)))
        }
    }
}

/// huggingface_hub の取得の進捗の行を、画面向けに直す（v0.5.7 項目 8、spec §6.0）。
///
/// hub 1.33（hf-xet）の大きなファイルは「`NAME: reconstructing file:  40%|███| 1.2GB / 3.06GB`」（全体と割合あり）と
/// 「`NAME: downloading bytes: ███ | 956MB, 24.6MB/s`」（量と速度だけ）を交互に出し、小さなファイルはいつもの tqdm
/// （「`NAME:  45%|███| 1.38G/3.06G [00:55<01:07, 25.0MB/s]`」）。どれも `\r` の上書きで、そのまま流すと棒グラフの文字が
/// 並ぶ。**割合が分かる行を優先**し、割合が変わったときだけ出す。割合の行が無いファイルは量が 50 MB 進むごとに出す。
#[derive(Default)]
struct HfProgress {
    shown_percent: std::collections::BTreeMap<String, u64>,
    has_percent: std::collections::BTreeSet<String>,
    shown_bytes: std::collections::BTreeMap<String, u64>,
}

/// 「`1.38G`」「`3.06GB`」「`956MB`」「`668`」を、バイト数へ（tqdm と hf-xet の書き方）。
fn parse_progress_size(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('B').trim_end_matches('i');
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1e3),
        'M' => (&s[..s.len() - 1], 1e6),
        'G' => (&s[..s.len() - 1], 1e9),
        'T' => (&s[..s.len() - 1], 1e12),
        _ => (s, 1.0),
    };
    num.trim().parse::<f64>().ok().map(|n| n * mult)
}

/// 画面向けの量（1 GB 以上は小数 2 桁の GB、それ未満は MB）。
fn human_size(bytes: f64) -> String {
    if bytes >= 1e9 {
        format!("{:.2} GB", bytes / 1e9)
    } else {
        format!("{:.0} MB", bytes / 1e6)
    }
}

impl HfProgress {
    /// 取得の進捗の行なら `Some`（中身は出す文言。今回は出さないなら `None`）。進捗でなければ `None`。
    fn describe(&mut self, line: &str) -> Option<Option<String>> {
        let (name, rest) = line.split_once(": ")?;
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None; // 「Warning: …」のような普通の行
        }
        if let Some(r) = rest.strip_prefix("reconstructing file:") {
            let (pct, sizes) = split_percent_and_sizes(r)?;
            let (done, total) = sizes.split_once(" / ")?;
            // 末尾にカーソルを上へ戻す並び（`\x1b[A`）や空白が付くので、最初のまとまりだけを読む
            let total = total.split_whitespace().next()?;
            let (done, total) = (parse_progress_size(done)?, parse_progress_size(total)?);
            self.has_percent.insert(name.to_string());
            return Some(self.percent_line(name, pct, done, total));
        }
        if let Some(r) = rest.strip_prefix("downloading bytes:") {
            if self.has_percent.contains(name) {
                return Some(None);
            }
            let after = r.rsplit_once('|')?.1;
            let (done, speed) = match after.split_once(',') {
                Some((d, sp)) => (d, sp.split_whitespace().next().map(str::to_string)),
                None => (after.split_whitespace().next().unwrap_or(""), None),
            };
            let done = parse_progress_size(done)?;
            const STEP: f64 = 50e6;
            let prev = self.shown_bytes.get(name).copied();
            if let Some(prev) = prev {
                if (done as u64) < prev + STEP as u64 {
                    return Some(None);
                }
            }
            self.shown_bytes.insert(name.to_string(), done as u64);
            return Some(Some(match speed {
                Some(sp) => format!("  取得中 {name} {}（{sp}）", human_size(done)),
                None => format!("  取得中 {name} {}", human_size(done)),
            }));
        }
        // いつもの tqdm（小さなファイル・hub 0.x）
        let (pct, sizes) = split_percent_and_sizes(rest)?;
        let sizes = sizes.split(" [").next()?;
        let (done, total) = sizes.split_once('/')?;
        let (done, total) = (parse_progress_size(done)?, parse_progress_size(total)?);
        self.has_percent.insert(name.to_string());
        Some(self.percent_line(name, pct, done, total))
    }

    fn percent_line(&mut self, name: &str, pct: u64, done: f64, total: f64) -> Option<String> {
        if self.shown_percent.get(name) == Some(&pct) {
            return None;
        }
        self.shown_percent.insert(name.to_string(), pct);
        Some(format!("  取得中 {name} {} / {}（{pct}%）", human_size(done), human_size(total)))
    }
}

/// 「`  40%|███   |  1.2GB / 3.06GB   …`」→ (40, 「1.2GB / 3.06GB   …」)。
fn split_percent_and_sizes(r: &str) -> Option<(u64, &str)> {
    let r = r.trim_start();
    let (pct, rest) = r.split_once('%')?;
    let pct: u64 = pct.trim().parse().ok()?;
    let after_bar = rest.strip_prefix('|')?.split_once('|')?.1;
    Some((pct, after_bar.trim()))
}

/// `pip install` を走らせる（共通の引数はここで付ける）。
///
/// - `--progress-bar raw`: パイプ越しだと pip は取得中の進捗を出さない（rich は端末でないと
///   描かない）ので、数 GB の torch の取得中は画面が止まって見えた。`raw` は進捗を行で出し、
///   `PipProgress` が画面向けに直す。**pip 24.1 からの選択肢**なので、入っている pip の版を
///   見てから付ける（知らない pip に渡すと、引数の誤りで導入そのものが止まる）
/// - `--disable-pip-version-check`: pip の新版の告知を出させない（失敗の理由の行のあとに続き、
///   ログの直前の出力を埋める）
fn run_pip_install<F>(py_exe: &Path, args: &[&str], on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let site = py_exe
        .parent()
        .map(|d| d.join("Lib").join("site-packages"))
        .unwrap_or_default();
    let mut full: Vec<&str> = vec![
        "-m",
        "pip",
        "install",
        "--no-warn-script-location",
        "--disable-pip-version-check",
    ];
    if pip_has_raw_progress(&site) {
        full.extend(["--progress-bar", "raw"]);
    }
    full.extend_from_slice(args);
    run_python(py_exe, &full, on_line)
}

/// 入っている pip が `--progress-bar raw` を知っているか（24.1 以降）。分からなければ偽。
fn pip_has_raw_progress(site: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(site) else {
        return false;
    };
    entries.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Some(version) = name
            .strip_prefix("pip-")
            .and_then(|r| r.strip_suffix(".dist-info"))
        else {
            return false;
        };
        let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
        match (parts.next().flatten(), parts.next().flatten()) {
            (Some(major), Some(minor)) => (major, minor) >= (24, 1),
            _ => false,
        }
    })
}

#[cfg(test)]
mod run_python_tests {
    use super::*;

    fn line(stream: Stream, text: &str) -> Line<'_> {
        Line {
            stream,
            text,
            overwritten: false,
        }
    }

    /// **pip の失敗の理由は `ERROR:` の行。** そのあとに補足と新版の告知が続くので、
    /// 最後の行を理由にすると「Check the permissions.」や告知を返してしまう。
    #[test]
    fn the_reason_of_a_pip_failure_is_its_error_line() {
        let mut tail = OutputTail::default();
        for (s, t) in [
            (Stream::Stdout, "Collecting torch"),
            (
                Stream::Stderr,
                "ERROR: Could not install packages due to an OSError: [WinError 5] アクセスが拒否されました。: 'c10.dll'",
            ),
            (Stream::Stderr, "Check the permissions."),
            (Stream::Stderr, "[notice] A new release of pip is available: 26.1.2 -> 26.2"),
        ] {
            tail.push(line(s, t));
        }
        assert_eq!(
            tail.reason(),
            Some("ERROR: Could not install packages due to an OSError: [WinError 5] アクセスが拒否されました。: 'c10.dll'")
        );
    }

    /// Python の例外は最終行が理由。**stdout の行は理由にしない**（届いた順に混ざるので、
    /// 例外のあとに stdout の書き残しが届きうる）。
    #[test]
    fn the_reason_of_a_traceback_is_its_exception_and_stdout_is_ignored() {
        let mut tail = OutputTail::default();
        for (s, t) in [
            (Stream::Stderr, "Traceback (most recent call last):"),
            (Stream::Stderr, "File \"<string>\", line 1, in <module>"),
            (Stream::Stderr, "ModuleNotFoundError: No module named 'pydub'"),
            (Stream::Stdout, "loading done"),
        ] {
            tail.push(line(s, t));
        }
        assert_eq!(tail.reason(), Some("ModuleNotFoundError: No module named 'pydub'"));
    }

    /// 失敗を述べる行が無ければ stderr の最後の行（モデル取得の失敗は `[hf-download]` の行）。
    #[test]
    fn without_an_error_line_the_last_stderr_line_is_the_reason() {
        let mut tail = OutputTail::default();
        tail.push(line(Stream::Stderr, "[hf-download] Aratako/Irodori-TTS-500M-v3@main/model.safetensors を確認中…"));
        tail.push(line(Stream::Stderr, "something odd happened"));
        assert_eq!(tail.reason(), Some("something odd happened"));
        tail.push(line(Stream::Stderr, "[hf-download] モデル DL 失敗: 404 Client Error"));
        tail.push(line(Stream::Stderr, "note: see above"));
        assert_eq!(tail.reason(), Some("[hf-download] モデル DL 失敗: 404 Client Error"));
    }

    /// 進捗の上書き（tqdm）はログに残さない。残す行数には上限がある。
    #[test]
    fn the_tail_skips_overwritten_progress_and_is_bounded() {
        let mut tail = OutputTail::default();
        tail.push(Line {
            stream: Stream::Stderr,
            text: "model.safetensors:  45%|####5     | 900M/2.00G",
            overwritten: true,
        });
        assert!(tail.lines.is_empty());
        assert_eq!(tail.reason(), None, "進捗の行を理由にしない");
        for i in 0..(FAILURE_TAIL_LINES + 5) {
            tail.push(line(Stream::Stdout, &format!("line {i}")));
        }
        assert_eq!(tail.lines.len(), FAILURE_TAIL_LINES);
        assert_eq!(tail.lines.back().map(String::as_str), Some("line 24"));
    }

    /// pip の raw の進捗は、割合が変わったときだけ画面向けの 1 行にする。進捗でない行は素通し。
    #[test]
    fn pip_raw_progress_is_shown_once_per_percent() {
        let mut p = PipProgress::default();
        let total = 100 * 1024 * 1024;
        assert_eq!(
            p.describe(&format!("Progress 0 of {total}")),
            Some(Some("  取得中 0 / 100 MB（0%）".to_string()))
        );
        assert_eq!(p.describe(&format!("Progress 1000 of {total}")), Some(None));
        assert_eq!(
            p.describe(&format!("Progress {} of {total}", total / 2)),
            Some(Some("  取得中 50 / 100 MB（50%）".to_string()))
        );
        // 次のファイルはまた 0% から
        assert_eq!(
            p.describe(&format!("Progress 0 of {total}")),
            Some(Some("  取得中 0 / 100 MB（0%）".to_string()))
        );
        assert_eq!(p.describe("Collecting torch"), None);
        assert_eq!(p.describe("Progress bar is fine"), None);
    }

    /// 大きさの分からない取得（`of 0`）は 10 MB ごとに出す。
    #[test]
    fn pip_raw_progress_without_a_size_is_shown_every_ten_megabytes() {
        let mut p = PipProgress::default();
        const MB: u64 = 1024 * 1024;
        assert!(matches!(p.describe("Progress 0 of 0"), Some(Some(_))));
        assert_eq!(p.describe(&format!("Progress {} of 0", 5 * MB)), Some(None));
        assert_eq!(
            p.describe(&format!("Progress {} of 0", 10 * MB)),
            Some(Some("  取得中 10 MB".to_string()))
        );
    }

    /// `--progress-bar raw` は pip 24.1 から。知らない pip に渡すと導入が止まるので、版を見る。
    #[test]
    fn raw_progress_is_used_only_with_a_pip_that_knows_it() {
        let has = |names: &[&str]| {
            let dir = tempfile::tempdir().unwrap();
            for n in names {
                std::fs::create_dir(dir.path().join(n)).unwrap();
            }
            pip_has_raw_progress(dir.path())
        };
        assert!(has(&["pip-26.1.2.dist-info"]));
        assert!(has(&["pip-24.1.dist-info"]));
        assert!(!has(&["pip-24.0.dist-info"]));
        assert!(!has(&["pip-23.3.2.dist-info"]));
        assert!(!has(&["pipx-25.0.dist-info", "torch-2.10.0.dist-info"]));
        assert!(!has(&[]), "pip が見当たらなければ付けない");
        assert!(!pip_has_raw_progress(Path::new("Z:/no/such/site-packages")));
    }

    #[test]
    fn exception_lines_are_recognised_but_prose_is_not() {
        assert!(is_failure_line("OSError: [WinError 5] アクセスが拒否されました。"));
        assert!(is_failure_line("huggingface_hub.errors.RepositoryNotFoundError: 404"));
        assert!(is_failure_line("ERROR: No matching distribution found for torch"));
        assert!(!is_failure_line("Successfully installed torch-2.10.0"));
        assert!(!is_failure_line("Note: this Error: is prose"));
        assert!(!is_failure_line("[hf-download] モデル DL 完了"));
    }

    /// **理由は stderr から選ぶ**（本物の経路で確かめる。`cmd.exe` を python の代わりに走らせる）。
    ///
    /// stdout の行をあとから届かせている: Python は stdout をまとめて吐き出すので、終了の直前に
    /// 届きうる。両方の最後の行を取ると、例外の行ではなくそちらを拾う。
    #[test]
    fn the_reason_comes_from_stderr_even_when_stdout_arrives_last() {
        let tag = std::process::id();
        let script = format!(
            "echo ModuleNotFoundError: No module named 'pydub' 1>&2& waitfor /t 2 UggLate{tag} >nul 2>&1& echo stdout-late& exit 1"
        );
        let got = failure_reason(Path::new("cmd.exe"), &["/d", "/c", &script]);
        assert_eq!(
            got.as_deref(),
            Some("ModuleNotFoundError: No module named 'pydub'")
        );
        // 成功したら理由は無い。
        assert_eq!(failure_reason(Path::new("cmd.exe"), &["/d", "/c", "echo ok"]), None);
    }

    /// **pip の進捗は書き換えて流し、失敗の理由には `ERROR:` の行を選ぶ**（本物の経路で確かめる）。
    #[test]
    fn progress_lines_are_rewritten_and_the_error_line_becomes_the_reason() {
        let script = "echo Progress 0 of 104857600& echo Collecting torch& echo ERROR: boom 1>&2& echo Check the permissions. 1>&2& exit 1";
        let mut seen: Vec<(bool, String)> = Vec::new();
        let err = run_python_lines(Path::new("cmd.exe"), &["/d", "/c", script], false, |l| {
            seen.push((l.overwritten, l.text.to_string()))
        })
        .expect_err("異常終了は Err");
        let texts: Vec<&str> = seen.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            !texts.contains(&"Progress 0 of 104857600"),
            "生の進捗の行を流している: {texts:?}"
        );
        assert!(
            seen.iter()
                .any(|(overwritten, t)| *overwritten && t.starts_with("  取得中 0 / 100 MB")),
            "書き換えた進捗の行が流れていない: {seen:?}"
        );
        assert!(texts.contains(&"Collecting torch"), "{texts:?}");
        let message = format!("{err:#}");
        assert!(message.contains("ERROR: boom"), "理由が載っていない: {message}");
        assert!(message.contains("code Some(1)"), "{message}");
    }

    /// 失敗のログに `-c` のスクリプトを丸ごと載せない。
    #[test]
    fn the_logged_arguments_omit_inline_scripts() {
        assert_eq!(
            describe_args(&["-c", "import json\nprint(1)"]),
            "-c <script>"
        );
        assert_eq!(
            describe_args(&["-m", "pip", "install", "torch"]),
            "-m pip install torch"
        );
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    fn pins_of(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn stamp_with_models(models: &[(&str, &str)]) -> InstalledStamp {
        InstalledStamp {
            schema: STAMP_SCHEMA,
            installed_at: 0,
            pins: Default::default(),
            resolved: Default::default(),
            requirements: Default::default(),
            models: pins_of(models),
        }
    }

    /// **読み先は記録から、名前ごとに決める**（v0.5.6 項目 3a の決定表）。
    ///
    /// 記録に無い名前は基準値（欄が空の記録が指す環境の中身）へ倒す。**記録全体ではなく名前ごと**に
    /// 引くのは、`models` が名前ごとに欠けうるため（入れ直した分しか書かないので、1 本だけ欠けた記録が作れる）。
    /// 記録全体を単位にすると、その 1 本の読み先が未定義になり、渡さなかった分は `sidecar.py` の既定値が使われる。
    #[test]
    fn the_read_targets_come_from_the_record_name_by_name() {
        // 記録が無い → 全部が基準値
        assert_eq!(models_to_read(None), v054_baseline_models());

        // 記録にある名前はその値、無い名前は基準値
        let stamp = stamp_with_models(&[("model_synth", "Aratako/Irodori-TTS-500M-v9@abc123")]);
        let got = models_to_read(Some(&stamp));
        assert_eq!(
            got.get("model_synth").map(String::as_str),
            Some("Aratako/Irodori-TTS-500M-v9@abc123"),
            "記録の値を読む"
        );
        assert_eq!(
            got.get("model_codec"),
            v054_baseline_models().get("model_codec"),
            "記録に無い名前は基準値へ倒す"
        );
        assert_eq!(got.len(), current_models().len(), "ビルドが求める名前は全部そろう");
    }

    /// 決定表の規則そのもの（**名前ごとに 記録 → 基準値 → ビルド**）。
    ///
    /// v0.5.6 では基準値といまのビルドが同じ値なので、本物の定数では規則を壊しても差が出ない。
    /// 3 つの表に**違う値**を入れて、どこから来たかを 1 つずつ確かめる。
    #[test]
    fn the_decision_table_picks_name_by_name() {
        let recorded = pins_of(&[("a", "記録のa")]);
        let baseline = pins_of(&[("a", "基準値のa"), ("b", "基準値のb")]);
        let build = pins_of(&[("a", "ビルドのa"), ("b", "ビルドのb"), ("c", "ビルドのc")]);

        let got = pick_models_to_read(&recorded, &baseline, &build);

        assert_eq!(got.get("a").map(String::as_str), Some("記録のa"), "記録が最優先");
        assert_eq!(
            got.get("b").map(String::as_str),
            Some("基準値のb"),
            "**記録があっても、その名前が無ければ基準値**（記録全体を単位にしない）"
        );
        assert_eq!(
            got.get("c").map(String::as_str),
            Some("ビルドのc"),
            "基準値にも無ければビルド（v0.5.7 でモデルを増やしたとき）"
        );
        assert_eq!(got.len(), 3, "ビルドが求める名前がそろう: {got:?}");

        // 記録が無い環境は全部が基準値（ビルドではない）
        let none = pick_models_to_read(&Default::default(), &baseline, &build);
        assert_eq!(none.get("a").map(String::as_str), Some("基準値のa"));
        assert_eq!(none.get("b").map(String::as_str), Some("基準値のb"));
    }

    /// **サイドカーの起動は「読み先」を渡すこと**（v0.5.6 項目 3a の配線）。
    ///
    /// 関数を取り違えても型は合うので、テストでは捕まらない（起動には実物の python が要る）。
    /// `sidecar.py` の既定値を見張っているのと同じやり方で、呼び出しの側をテキストで固定する。
    #[test]
    fn the_sidecar_is_started_with_the_read_targets() {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/sidecar.rs"),
        )
        .expect("sidecar.rs を読めること").replace("\r\n", "\n");
        assert!(
            src.contains("model_args_for_read(asset_root)"),
            "起動が読み先を使っていない"
        );
        assert!(
            !src.contains("model_args_for_fetch"),
            "起動に取得先を渡している（重みが無いモデルを読みに行く）"
        );
    }

    /// 読み先をどこから決めたかを言い分ける（実機で追う観測点）。
    #[test]
    fn the_source_of_the_read_targets_is_named() {
        assert_eq!(where_models_come_from(None), "基準値");
        let full: Vec<(&str, &str)> = V054_BASELINE_MODELS.to_vec();
        assert_eq!(where_models_come_from(Some(&stamp_with_models(&full))), "記録");
        assert_eq!(
            where_models_come_from(Some(&stamp_with_models(&full[..1]))),
            "記録と基準値"
        );
    }

    /// **取得する先と読む先は別**（v0.5.6 項目 3a）。取得はいまのビルド、読みは記録。
    /// 同じ値を両方へ渡していたため、定数を変えた版を入れた瞬間に重みの無いモデルを読みに行っていた。
    #[test]
    fn what_is_fetched_and_what_is_read_can_differ() {
        let dir = tempfile::tempdir().unwrap();
        write_stamp_pins(
            dir.path(),
            Default::default(),
            Default::default(),
            Default::default(),
            pins_of(&[
                ("model_synth", "Aratako/Irodori-TTS-500M-v3@old111"),
                (
                    "model_voice_design",
                    "Aratako/Irodori-TTS-500M-v2-VoiceDesign@main",
                ),
                ("model_codec", "Aratako/Semantic-DACVAE-Japanese-32dim@main"),
            ]),
        )
        .unwrap();

        let (read, from) = model_args_for_read(dir.path());
        assert_eq!(from, "記録");
        let read = read.join(" ");
        assert!(read.contains("--model-synth-revision old111"), "{read}");
        let fetch = model_args_for_fetch().join(" ");
        let synth_rev = MODEL_PINS.iter().find(|(n, _, _)| *n == "model_synth").unwrap().2;
        assert!(fetch.contains(&format!("--model-synth-revision {synth_rev}")), "{fetch}");
        assert_ne!(read, fetch, "記録が古ければ読み先と取得先は違う");
    }

    /// **書いている最中に読んでも、記録は壊れて見えない**（v0.5.6 項目 3a）。
    ///
    /// 上書きで書くと truncate と書き込みの間が読めてしまい、`read_stamp` はそれを「記録なし」に畳む。
    /// 読み先を記録から決めるようになったので、そのとき読み先が基準値へ倒れる（v0.5.7 では
    /// 更新に成功した環境が旧モデルを読みに行き、合成が無言で VOICEVOX へ落ちる）。
    #[test]
    fn a_record_being_written_is_never_read_half_done() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let write = |tag: &str| {
            write_stamp_pins(
                &root,
                pins_of(&[("python", "3.11.9")]),
                pins_of(&[("torch", tag)]),
                Default::default(),
                pins_of(&[("model_synth", "Aratako/Irodori-TTS-500M-v3@main")]),
            )
            .unwrap()
        };
        write("2.10.0");

        let reading = root.clone();
        let reader = std::thread::spawn(move || {
            let mut missing = 0;
            for _ in 0..400 {
                if read_stamp(&reading).is_none() {
                    missing += 1;
                }
            }
            missing
        });
        for i in 0..200 {
            write(if i % 2 == 0 { "2.10.0" } else { "2.10.1" });
        }
        let missing = reader.join().unwrap();
        assert_eq!(missing, 0, "書いている最中の記録が {missing} 回読めなかった");
    }

    /// 記録の書き込みは差し替えで行う（書きかけを残さない）。
    #[test]
    fn the_record_is_replaced_not_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        write_stamp_pins(
            dir.path(),
            pins_of(&[("python", "3.11.9")]),
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .unwrap();
        assert!(read_stamp(dir.path()).is_some(), "書いた記録が読めること");
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [STAMP_FILE], "書きかけを残さない: {left:?}");
    }

    /// **記録が無い環境を「最新」と言わない** (v0.5.4 項目 2)。
    ///
    /// v0.5.4 より前に導入した環境には記録が無い。そこを「一致」と読むと、
    /// **この機能が対象にしている当のユーザー**（pin が届いていない人）を取りこぼす。
    #[test]
    fn no_record_is_not_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let st = status(dir.path());
        assert!(!st.has_record, "記録が無いこと");
        assert!(!st.up_to_date, "分からないものを最新とは言わない");
    }

    /// **「使える」と「最新」は別の信号** (v0.5.4 項目 2 の訂正)。
    ///
    /// 古いランタイムでも動いている以上 `present` は真のまま。ここを偽にすると、
    /// フロントの `canUseReal = gpuOk && assetsOk` が倒れ、ユーザーの
    /// `tts_irodori_use_real_model` が黙って false に書き換えられて永続化される。
    #[test]
    fn present_does_not_depend_on_being_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("python").join("Lib").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();
        for pkg in ["torch", "fastapi", "uvicorn", "huggingface_hub", "irodori_tts"] {
            std::fs::create_dir_all(site.join(pkg)).unwrap();
        }
        // 記録は無い（= 古い導入）が、パッケージは揃っている。
        let st = status(dir.path());
        assert!(st.present, "揃っているなら使える");
        assert!(!st.up_to_date, "記録が無いので最新とは言えない");
    }

    /// 記録した pin が現在の pin と一致すれば最新、違えばその名前を返す。
    #[test]
    fn outdated_pins_names_what_changed() {
        let recorded = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-1")]);
        let same = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-1")]);
        assert!(outdated_pins(&recorded, &same).is_empty());

        let bumped = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-2")]);
        assert_eq!(outdated_pins(&recorded, &bumped), ["irodori_tts"]);

        // pin を増やした場合も「入れ直しが要る」
        let added = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-1"), ("newdep", "N-1")]);
        assert_eq!(outdated_pins(&recorded, &added), ["newdep"]);

        // pin を減らした場合は入れ直し不要（記録側にしか無いものは無視する）
        let removed = pins_of(&[("python", "PY-1")]);
        assert!(outdated_pins(&recorded, &removed).is_empty());
    }

    /// 書いた記録を読み戻せること。**記録できても読めなければ記録ではない。**
    #[test]
    fn stamp_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        assert!(read_stamp(dir.path()).is_none(), "書く前は記録なし");

        let resolved = [("transformers", "4.57.6"), ("huggingface_hub", "0.36.2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        write_stamp_pins(
            dir.path(),
            current_pins(),
            resolved,
            current_requirements(),
            current_models(),
        )
        .unwrap();

        let got = read_stamp(dir.path()).expect("読み戻せること");
        assert_eq!(got.pins, current_pins(), "要求した pin をそのまま記録する");
        assert_eq!(got.resolved.get("huggingface_hub").map(String::as_str), Some("0.36.2"));
        assert!(status(dir.path()).up_to_date, "書いた直後は最新");
    }

    /// **記録するのは「指定した版」ではなく「実際に入った版」** (v0.5.4)。
    ///
    /// v0.5.6 までは `huggingface_hub==0.27.0` と指定しているのに実機は 0.36.2 だった
    /// （transformers の依存に押し上げられた）。この食い違いを記録できなければ、
    /// 「版を固定して再現性を担保」という宣言が成立していないことに気づけない。
    /// v0.5.7 で指定を実物に合わせた（1.33.0）後も、記録が実測値を残す規則は変わらない。
    #[test]
    fn resolved_can_differ_from_the_requested_pin() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = [("huggingface_hub", "0.36.2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        write_stamp_pins(
            dir.path(),
            current_pins(),
            resolved,
            current_requirements(),
            current_models(),
        )
        .unwrap();

        let got = read_stamp(dir.path()).unwrap();
        assert!(
            COMMON_REQUIREMENTS
                .iter()
                .any(|r| *r == "huggingface_hub==1.33.0"),
            "前提: 指定は 1.33.0（記録した実測の 0.36.2 と違う）"
        );
        assert_eq!(
            got.resolved.get("huggingface_hub").map(String::as_str),
            Some("0.36.2"),
            "実測値をそのまま残すこと"
        );
    }

    /// スキーマが違う記録は「記録なし」として扱う（読めない形を最新と誤認しない）。
    #[test]
    fn unknown_schema_is_treated_as_no_record() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STAMP_FILE),
            r#"{"schema":999,"installed_at":0,"pins":{},"resolved":{}}"#,
        )
        .unwrap();
        assert!(read_stamp(dir.path()).is_none());
        assert!(!status(dir.path()).up_to_date);
    }

    /// 要件文字列から配布名を取り出す。
    #[test]
    fn requirement_name_strips_specifiers() {
        assert_eq!(requirement_name("torch>=2.10.0,<2.11.0"), "torch");
        assert_eq!(requirement_name("uvicorn[standard]==0.32.1"), "uvicorn");
        assert_eq!(requirement_name("numpy<2"), "numpy");
        assert_eq!(requirement_name("einops"), "einops");
        assert_eq!(requirement_name("descript-audiotools>=0.7.2"), "descript-audiotools");
    }

    /// 記録対象に、**pip で名前指定して入れたもの全部と GitHub 由来の 3 本**が入る。
    /// 取りこぼすと「何が入っているか」の記録として欠ける。
    #[test]
    fn recorded_distributions_cover_everything_we_install() {
        let names = recorded_distributions();
        for expected in ["torch", "transformers", "huggingface_hub", "fastapi", "numpy"] {
            assert!(names.iter().any(|n| n == expected), "{expected} が記録対象に無い");
        }
        for git in ["silentcipher", "dacvae", "irodori-tts"] {
            assert!(names.iter().any(|n| n == git), "{git} が記録対象に無い");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "重複がある: {names:?}");
    }

    /// python から返る解決済みバージョン行を読める。
    #[test]
    fn resolved_line_is_parsed() {
        let line = format!("{VERSIONS_MARKER}{{\"torch\":\"2.10.0+cu128\"}}");
        let got = parse_resolved_line(&line).expect("読めること");
        assert_eq!(got.get("torch").map(String::as_str), Some("2.10.0+cu128"));
        assert!(parse_resolved_line("pip install ...").is_none(), "無関係な行は拾わない");
    }

    /// **記録が無いことは「Python の版が違う」ことではない** (v0.5.4、実機検証の直前に発見)。
    ///
    /// 記録の欠落を「全 pin が古い」と読み替えて対象に `python` を混ぜていたため、
    /// 更新経路が冒頭の Err で止まり、**この機能が対象にしている環境
    /// （= v0.5.4 より前に導入した環境）がちょうど 1 つも更新できなかった**。
    /// しかもユーザーには「Python 本体の版が変わっています。全部消して入れ直せ」という
    /// 事実でない案内が出ていた。
    #[test]
    fn a_missing_record_does_not_accuse_python() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("python").join("Lib").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        // 起動できない python.exe = 版を聞けない状況。聞けないなら古いとは言わない。
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();
        for pkg in ["torch", "fastapi", "uvicorn", "huggingface_hub", "irodori_tts"] {
            std::fs::create_dir_all(site.join(pkg)).unwrap();
        }

        let st = status(dir.path());
        assert!(st.present && !st.has_record && !st.up_to_date, "前提: 古い導入");
        assert!(
            !st.outdated.iter().any(|n| n == "python"),
            "聞けなかった python を対象に混ぜてはいけない: {:?}",
            st.outdated
        );
        for pkg in ["silentcipher", "dacvae", "irodori_tts"] {
            assert!(
                st.outdated.iter().any(|n| n == pkg),
                "{pkg} が入れ直しの対象から漏れている: {:?}",
                st.outdated
            );
        }
    }

    /// pin した URL から版を取り出せる（ここが壊れると python の判定が常に「聞けない」に倒れる）。
    #[test]
    fn pinned_python_version_comes_from_the_url() {
        assert_eq!(pinned_python_version(), Some("3.11.9"));
        assert!(PYTHON_URL.contains("python-3.11.9-embed-amd64.zip"), "前提が変わったら上も直す");
    }

    /// **証拠が無いほうへ倒す。** 聞けなかった版を「違う」と扱うと、直せもしない
    /// 全削除・再導入（数 GB）をユーザーに要求することになる。
    #[test]
    fn python_staleness_needs_evidence() {
        assert!(!python_is_stale(Some("3.11.9"), Some("3.11.9")), "一致なら古くない");
        assert!(python_is_stale(Some("3.11.8"), Some("3.11.9")), "違えば古い");
        assert!(!python_is_stale(None, Some("3.11.9")), "聞けないなら古いと言わない");
        assert!(!python_is_stale(Some("3.11.9"), None), "pin を読めないなら古いと言わない");
    }

    /// **初回導入の経路も、確認できない `python` を「最新」と記録しない**（監査 ④）。
    ///
    /// `ensure_python_embeddable` は `python.exe` があれば skip するので、
    /// 「入れた」と「入っている」は一致しない。ここで無条件に記録すると、以後
    /// `should_ask_python` が「記録が一致」と判断して実物に聞かなくなり、
    /// **古い python が永久に見えなくなる**。更新経路だけ直して隣に残していた穴。
    #[test]
    fn the_install_path_does_not_claim_an_unverified_python() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("python")).unwrap();
        // 起動できない python.exe = 版を確かめられない状況
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();

        record_installed(dir.path(), |_| {}).expect("記録は書けること");

        let pins = read_stamp(dir.path()).expect("記録があること").pins;
        assert!(
            !pins.contains_key("python"),
            "確かめられていない python を記録してはいけない: {pins:?}"
        );
        for pkg in ["dacvae", "irodori_tts", "silentcipher"] {
            assert_eq!(pins.get(pkg), current_pins().get(pkg), "{pkg} は記録すること");
        }
        assert!(
            should_ask_python(&pins),
            "記録していないのだから、次回は実物に聞きにいくこと"
        );
    }

    /// **初回導入は、要件とモデルも全部記録する**（2026-09-14 監査で発覚）。
    ///
    /// 以前は固定 URL の 3 本しか記録せず、要件・モデルの欄は空のまま書かれて、あとで基準値を
    /// 書き足す処理に頼っていた。書き足す値が「いまのビルドの値」だったため、要件を変えた
    /// ビルドで差が出ず更新が届かない穴の入口になっていた。
    #[test]
    fn the_install_path_records_every_requirement_and_model() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("python")).unwrap();
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();

        record_installed(dir.path(), |_| {}).expect("記録は書けること");

        let stamp = read_stamp(dir.path()).expect("記録があること");
        assert_eq!(stamp.requirements, current_requirements(), "要件を全部記録する");
        assert_eq!(stamp.models, current_models(), "モデルを全部記録する");
    }

    /// **入れ直せた分だけ**を現在値へ反映する。全部を現在値にすると、入れ直していない
    /// 依存まで「最新」と記録して記録そのものが嘘になる。
    #[test]
    fn only_updated_pins_are_recorded() {
        let updated = vec!["dacvae".to_string()];
        let pins = merged_pins(&Default::default(), &updated, false);
        assert_eq!(pins.len(), 1, "入れ直した 1 本だけ: {pins:?}");
        assert_eq!(pins.get("dacvae"), current_pins().get("dacvae"));
        assert!(!pins.contains_key("python"), "確認していない python を書かない");
        assert!(!pins.contains_key("irodori_tts"), "入れ直していないものを書かない");

        // 実物が pin と一致していると確認できたときだけ python も記録する
        let pins = merged_pins(&Default::default(), &updated, true);
        assert_eq!(pins.get("python"), current_pins().get("python"));

        // 3 本入れ直し + python 確認済み = 最新になる
        let all: Vec<String> = ["dacvae", "irodori_tts", "silentcipher"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pins = merged_pins(&Default::default(), &all, true);
        assert!(
            outdated_pins(&pins, &current_pins()).is_empty(),
            "全部入れ直したら最新になること: {pins:?}"
        );
    }

    /// **入れ直せた要件だけを記録する** (v0.5.5 項目 3)。
    ///
    /// `merged_pins` と同じ規律。全部を現在値にすると、入れ直していない依存まで
    /// 「最新」と記録して記録そのものが嘘になる。**`pins` 側だけ守って隣を忘れない。**
    #[test]
    fn only_updated_requirements_are_recorded() {
        let updated = vec!["transformers".to_string()];
        let reqs = merged_requirements(&Default::default(), &updated);
        assert_eq!(reqs.len(), 1, "入れ直した 1 本だけ: {reqs:?}");
        assert_eq!(reqs.get("transformers"), current_requirements().get("transformers"));
        assert!(
            !reqs.contains_key("huggingface_hub"),
            "入れ直していないものを書かない: {reqs:?}"
        );

        // 古い記録は入れ直すまで古いまま残る
        let mut recorded = std::collections::BTreeMap::new();
        recorded.insert("numpy".to_string(), "numpy<1".to_string());
        let reqs = merged_requirements(&recorded, &updated);
        assert_eq!(
            reqs.get("numpy").map(String::as_str),
            Some("numpy<1"),
            "触っていない記録を書き換えない: {reqs:?}"
        );
    }

    /// **torch は CUDA index から入れ直す** (v0.5.5 項目 3)。
    ///
    /// 名前だけで `pip install` すると PyPI の **CPU 版**が入り、GPU 合成が黙って壊れる。
    /// 初回導入（`install_torch_cuda`）は `--index-url` を付けているので、入れ直しでも揃える。
    #[test]
    fn torch_is_reinstalled_from_the_cuda_index() {
        assert!(needs_torch_index("torch"), "torch は CUDA index が要る");
        assert!(needs_torch_index("torchaudio"), "torchaudio も同じ index");
        for other in ["transformers", "huggingface_hub", "numpy", "torchcodec"] {
            assert!(
                !needs_torch_index(other),
                "{other} に CUDA index を付けると取得先を誤る"
            );
        }
    }

    /// **名前付き pip 要件の版を変えたら更新対象になる** (v0.5.5 項目 3)。
    ///
    /// v0.5.4 の判定は `current_pins()`（固定 URL 4 本）としか突き合わせておらず、
    /// `transformers<5` を `>=5` に変えても `outdated` は空のまま＝**更新ボタンすら
    /// 出なかった**。transformers 4→5 を届ける手段が 1 本も無い状態だった。
    #[test]
    fn changing_a_named_requirement_makes_it_outdated() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        let pins = current_pins();
        let mut reqs = current_requirements();
        assert!(
            outdated_list(dir.path(), &pins, &reqs, &current_models()).is_empty(),
            "全部一致なら空"
        );

        // 記録されているのは**古い要件**。いまのビルドは違うものを要求している
        // ＝ 既存環境へ届けなければならない。
        assert_eq!(
            current_requirements().get("transformers").map(String::as_str),
            Some("transformers==5.17.0"),
            "前提が変わったらこのテストも直す"
        );
        reqs.insert("transformers".to_string(), "transformers<4".to_string());
        let got = outdated_list(dir.path(), &pins, &reqs, &current_models());
        assert!(
            got.iter().any(|n| n == "transformers"),
            "要件の変更が対象に入らないと、永久に届かない: {got:?}"
        );
    }

    /// **欄が空の記録は、v0.5.4 の基準値が入っているとみなす** (v0.5.5、2026-09-14 監査で改めた)。
    ///
    /// 固定 URL の 3 本は「記録が無い＝ pin 前の `refs/heads/main` が入っている」と
    /// 実機で確認できているので全部対象にしてよい。**要件とモデルは違う** — 記録に
    /// 欄が無いのは v0.5.4 以前が書いた記録だからで、中身が古い証拠にはならない。
    /// ここを対象にすると、v0.5.4 から上げただけのユーザーに **torch を含む数 GB の
    /// 再取得**を強いる。基準値で読むので、対象になるのは**このビルドが基準値から変えたもの
    /// だけ**（v0.5.5・v0.5.6 は何も変えていないので空、v0.5.7 は transformers などを変えた）。
    #[test]
    fn a_missing_record_does_not_accuse_requirements_or_models() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        let got = outdated_list(
            dir.path(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        for name in ["torch", "torchaudio", "fastapi", "numpy"] {
            assert!(
                !got.iter().any(|n| n == name),
                "{name} を数 GB かけて入れ直す理由が無い: {got:?}"
            );
        }
        let pins = ["dacvae", "irodori_tts", "silentcipher"];
        let named: Vec<String> = got.iter().filter(|n| !pins.contains(&n.as_str())).cloned().collect();
        assert_eq!(
            named,
            changed_since_baseline(),
            "要件とモデルで対象になるのは、基準値から変えたものだけ"
        );
        // 固定 URL の 3 本は従来どおり対象（記録が無い＝ pin 前が入っていると分かっている）
        for pkg in ["dacvae", "irodori_tts", "silentcipher"] {
            assert!(got.iter().any(|n| n == pkg), "{pkg} は対象のまま: {got:?}");
        }
    }

    /// **`status()` を通しても基準値が入ること**（関数だけ作って呼び忘れない）。
    ///
    /// `backfill_baseline` 単体のテストでは、`status()` から呼ばれているかまでは
    /// 固定できない。呼ばれていなければ、v0.5.4 の記録は欄が空のまま残り、
    /// **次に要件を変えても永久に届かない**。
    #[test]
    fn status_writes_the_baseline_into_an_old_record() {
        let dir = tempfile::tempdir().unwrap();
        write_stamp_pins(
            dir.path(),
            current_pins(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .unwrap();

        let st = status(dir.path());
        assert!(st.has_record, "前提: 記録はある");

        let persisted = read_stamp(dir.path()).expect("記録が読めること");
        assert_eq!(
            persisted.requirements,
            v054_baseline_requirements(),
            "status() から基準値が書かれていない（呼び忘れ）"
        );
        assert_eq!(persisted.models, v054_baseline_models(), "モデルの基準値も同じ");
    }

    /// **基準値を書き足す** (v0.5.5)。
    ///
    /// これが無いと、v0.5.4 が書いた記録は要件の欄が空のままで、**次に要件を変えても
    /// 差が出ず永久に届かない**（v0.5.5 がまさに直した穴の再発）。
    #[test]
    fn an_old_record_gets_a_baseline_written_in() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        // v0.5.4 が書いた記録 = pins と resolved はあるが requirements / models が無い
        write_stamp_pins(
            dir.path(),
            current_pins(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let before = read_stamp(dir.path()).unwrap();
        assert!(before.requirements.is_empty() && before.models.is_empty(), "前提");

        let after = backfill_baseline(dir.path(), &before);
        assert_eq!(after.requirements, v054_baseline_requirements(), "要件の基準値が入る");
        assert_eq!(after.models, v054_baseline_models(), "モデルの基準値が入る");

        // **書き込まれて残ること**（次回の判定で使えなければ意味が無い）
        let persisted = read_stamp(dir.path()).unwrap();
        assert_eq!(persisted.requirements, v054_baseline_requirements(), "保存されていない");

        // 基準値が入ったあとは、このビルドが基準値から変えたものだけが対象になる
        let base = outdated_list(dir.path(), &persisted.pins, &persisted.requirements, &persisted.models);
        assert_eq!(base, changed_since_baseline(), "基準値から変えたものだけ: {base:?}");
        // 記録の側で 1 本だけ違えば、その 1 本が足される（ほかは巻き込まない）
        let mut reqs = persisted.requirements.clone();
        reqs.insert("einops".to_string(), "einops<0".to_string());
        let got = outdated_list(dir.path(), &persisted.pins, &reqs, &persisted.models);
        let mut expected = base.clone();
        expected.push("einops".to_string());
        expected.sort();
        assert_eq!(got, expected, "変えた 1 本だけ足される: {got:?}");
    }

    /// いまのビルドのモデルのファイルを置く（v0.5.7 項目 2）。記録がいまの値を指していても、
    /// ファイルが無ければ「更新が要る」に数えるので、「全部一致なら空」を確かめるテストはこれを先に呼ぶ。
    fn place_current_model_files(root: &Path) {
        for (name, repo, rev) in MODEL_PINS {
            let dir = root.join("model").join(model_dir_name(repo, rev));
            let files = MODEL_FILES.iter().find(|(n, _)| n == name).expect("表に無い").1;
            for f in files {
                let path = dir.join(f);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"x").unwrap();
            }
        }
    }

    /// **記録はいまのモデルを指しているのにトークナイザが欠けていたら、「更新が要る」に数える**（v0.5.7 項目 2）。
    /// ただし「未導入」にはしない — `assets_ready` はモデルを見ない（見ると更新前の人の設定が消え、
    /// 更新ボタンも隠れる。spec §6.0 の反証 1）。
    #[test]
    fn a_missing_tokenizer_makes_the_model_outdated_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        let (_, repo, rev) = MODEL_PINS.iter().find(|(n, _, _)| *n == "model_synth").unwrap();
        let synth = dir.path().join("model").join(model_dir_name(repo, rev));
        std::fs::remove_file(synth.join("tokenizer").join("tokenizer_config.json")).unwrap();

        let got = outdated_list(dir.path(), &current_pins(), &current_requirements(), &current_models());
        // 同じ置き場所を指す名前（項目 3 から、合成と参照音声の生成は同じ v4.1）がそろって対象になる
        let expected: Vec<String> = MODEL_PINS
            .iter()
            .filter(|(_, r, v)| (*r, *v) == (*repo, *rev))
            .map(|(n, _, _)| n.to_string())
            .collect();
        assert!(expected.contains(&"model_synth".to_string()));
        assert_eq!(got, expected, "欠けたモデルだけが対象: {got:?}");
        assert!(!assets_ready(dir.path()), "前提: この一時フォルダには Python が無い");
        // `assets_ready` の中身はモデルを見ない（本文のテキストで固定）
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("pub fn assets_ready(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(
            !body.contains("MODEL_FILES") && !body.contains("incomplete_models") && !body.contains("tokenizer"),
            "assets_ready がモデルを見ている（「使えるか」と「最新か」を混ぜない）"
        );
    }

    /// 記録が**古いモデル**を指しているときは、新しいモデルのファイルが無いのは当たり前なので、
    /// 欠けを理由に数えない（古いことを理由に数える。二重に数えない）。
    #[test]
    fn an_old_model_in_the_record_is_not_blamed_for_missing_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = v054_baseline_models();
        assert!(incomplete_models(dir.path(), &old).iter().all(|n| n == "model_voice_design"),
            "古い値の欄は見ない（いまの値と同じ欄だけが残りうる）");
        let got = outdated_list(dir.path(), &current_pins(), &current_requirements(), &old);
        let mut expected = changed_since_baseline()
            .into_iter()
            .filter(|n| n.starts_with("model_"))
            .collect::<Vec<_>>();
        // いまの値と同じ欄（参照音声の生成）はファイルが無いので欠けとして数える
        expected.push("model_voice_design".to_string());
        expected.sort();
        expected.dedup();
        assert_eq!(got, expected);
    }

    /// 全部そろっていれば数えない。
    #[test]
    fn complete_current_models_are_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        assert!(incomplete_models(dir.path(), &current_models()).is_empty());
    }

    /// **置き場所の名前は `sidecar.py` の `model_dir_name` と同じ規則**（v0.5.7 項目 2。Rust で欠けを見る
    /// ために写した。食い違うと、揃っているのに毎回「更新が要る」になるか、欠けを見逃す）。
    #[test]
    fn the_model_dir_name_matches_the_sidecar() {
        assert_eq!(model_dir_name("Aratako/X", "main"), "Aratako__X");
        assert_eq!(model_dir_name("Aratako/X", ""), "Aratako__X");
        assert_eq!(model_dir_name("Aratako/X", "abc"), "Aratako__X@abc");
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("python").join("sidecar.py"))
            .unwrap().replace("\r\n", "\n");
        for needle in [
            "safe = repo.replace(\"/\", \"__\")",
            "return f\"{safe}@{revision}\" if revision and revision != \"main\" else safe",
            "WEIGHT_FILE_PATTERNS = (\"model.safetensors\", \"tokenizer/*\")",
        ] {
            assert!(src.contains(needle), "sidecar.py の規則が変わった: {needle}");
        }
        // 表の名前は MODEL_PINS と同じ組
        let mut a: Vec<&str> = MODEL_PINS.iter().map(|(n, _, _)| *n).collect();
        let mut b: Vec<&str> = MODEL_FILES.iter().map(|(n, _)| *n).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    /// このビルドが v0.5.4 の基準値から変えた要件とモデルの名前（並べ替え済み）。
    fn changed_since_baseline() -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (current, baseline) in [
            (current_requirements(), v054_baseline_requirements()),
            (current_models(), v054_baseline_models()),
        ] {
            out.extend(
                current
                    .iter()
                    .filter(|(name, value)| baseline.get(*name) != Some(value))
                    .map(|(name, _)| name.clone()),
            );
        }
        out.sort();
        out
    }

    /// **欄が空の記録を「いまの要求どおり」と読まない**（2026-09-14 監査で発覚）。
    ///
    /// 読んでしまうと、要件を変えたビルドで差が出ず、**v0.5.4 から上げただけの環境に
    /// 更新が永久に届かない**。v0.5.5 のうちは基準値といまの値が一致して鳴らないので、
    /// 食い違う状況を作って確かめる。
    #[test]
    fn an_empty_section_is_read_as_the_frozen_baseline() {
        let baseline = pins_of(&[("transformers", "transformers<5")]);
        let current = pins_of(&[("transformers", "transformers>=5")]);
        assert_eq!(
            outdated_section(&Default::default(), &baseline, &current),
            ["transformers"],
            "欄が空でも、当時の版といまの要求が違えば届ける"
        );
        assert!(
            outdated_section(&Default::default(), &current, &current).is_empty(),
            "当時の版のままでよければ何もしない（数 GB の再取得を強いない）"
        );
    }

    /// **あとから増えた要件も届ける**（2026-09-14 監査の掃討で発覚）。
    ///
    /// 以前は「記録に名前が無いものは古いと扱わない」だったので、要件を新しく足しても
    /// 既存環境には入らなかった。欄が埋まった記録で名前が無いのは、後から増えたものだけ。
    #[test]
    fn a_requirement_added_later_is_delivered() {
        let recorded = pins_of(&[("transformers", "transformers<5")]);
        let current = pins_of(&[("transformers", "transformers<5"), ("newdep", "newdep>=1")]);
        assert_eq!(outdated_section(&recorded, &recorded, &current), ["newdep"]);
    }

    /// **基準値で埋めるのは欄が空のときだけ、埋めるのは固定の基準値**（2026-09-14 監査で改めた）。
    ///
    /// いまのビルドの値で埋めると、要件を変えたビルドで差が出ず更新が届かない。
    /// 欄が埋まっている記録（その時のビルドが全部を記録したもの）には触らない。
    #[test]
    fn the_baseline_fills_only_an_empty_section_with_the_frozen_values() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_reqs = pins_of(&[("transformers", "transformers<5")]);
        let baseline_models = pins_of(&[("model_synth", "old@main")]);

        write_stamp_pins(
            dir.path(),
            current_pins(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let empty = read_stamp(dir.path()).unwrap();
        let filled = baseline_filled(&empty, &baseline_reqs, &baseline_models);
        assert_eq!(filled.requirements, baseline_reqs, "渡された基準値で埋める");
        assert_eq!(filled.models, baseline_models);

        let recorded = pins_of(&[("transformers", "transformers>=5")]);
        write_stamp_pins(
            dir.path(),
            current_pins(),
            Default::default(),
            recorded.clone(),
            current_models(),
        )
        .unwrap();
        let full = read_stamp(dir.path()).unwrap();
        let untouched = baseline_filled(&full, &baseline_reqs, &baseline_models);
        assert_eq!(untouched.requirements, recorded, "埋まっている欄は書き換えない");
        assert_eq!(untouched.models, current_models());
    }

    /// **v0.5.6 までの環境から見て、v0.5.7 で変えた要件が漏れなく「更新が要る」に入る**（v0.5.7 項目 1）。
    ///
    /// v0.5.6 までは基準値といまの要求が一致しており、`the_frozen_baseline_was_copied_correctly` が
    /// その等式で写し間違いを見張っていた（v0.5.7 で要件を変えたら消す、と決めてあった）。等式が崩れた
    /// いまは、**崩れ方**を固定する: 基準値（＝ v0.5.4〜v0.5.6 が入れた版）から見て、要件の差は
    /// 版を上げた 3 つと新しく足した 1 つだけ。ほかの要件を巻き込まない（巻き込むと torch を含む数 GB を
    /// 取り直す）。**`V054_BASELINE_*` の定数は変えない。**
    #[test]
    fn the_v057_requirement_changes_reach_a_v056_install() {
        let baseline = v054_baseline_requirements();
        let current = current_requirements();
        let mut got = outdated_section(&baseline, &baseline, &current);
        got.sort();
        assert_eq!(
            got,
            vec!["huggingface_hub", "pydub", "tokenizers", "transformers"],
            "v0.5.6 の環境で入れ直すのはこの 4 つだけ"
        );
        // 記録の欄が空の環境（v0.5.4 が書いた記録・記録なし）も、基準値で読んで同じ結果になる。
        let mut from_empty = outdated_section(&Default::default(), &baseline, &current);
        from_empty.sort();
        assert_eq!(from_empty, got);
        // 基準値の側は据え置き（v0.5.4〜v0.5.6 が入れていた版のまま）。
        assert_eq!(
            baseline.get("transformers").map(String::as_str),
            Some("transformers<5")
        );
        assert_eq!(
            baseline.get("huggingface_hub").map(String::as_str),
            Some("huggingface_hub==0.27.0")
        );
        // モデルは 3 つとも変えた（項目 2 で合成とコーデック、項目 3 で参照音声の生成）。
        let mut models = outdated_section(
            &v054_baseline_models(),
            &v054_baseline_models(),
            &current_models(),
        );
        models.sort();
        assert_eq!(models, vec!["model_codec", "model_synth", "model_voice_design"]);
    }

    /// **v0.5.7 の要件の入れ直しは、torch の index を使わない 1 回の pip で行う**（v0.5.6 項目 3c の段取り）。
    ///
    /// huggingface_hub と transformers と tokenizers は互いの版を縛り合うので、別々の pip に分けると
    /// 途中の組み合わせで解決に失敗しうる（spike で確かめたのは 4 つを 1 回で入れた組み合わせ）。
    #[test]
    fn the_v057_requirements_go_in_one_pip_call() {
        let outdated: Vec<String> = ["huggingface_hub", "pydub", "tokenizers", "transformers"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let plan = update_plan(&outdated);
        assert!(plan.torch.is_empty(), "torch の index から入れるものは無い: {plan:?}");
        let mut other = plan.other.clone();
        other.sort();
        assert_eq!(
            other,
            vec![
                "huggingface_hub==1.33.0",
                "pydub==0.25.1",
                "tokenizers==0.23.2",
                "transformers==5.17.0",
            ]
        );
        assert!(plan.skipped.is_empty());
    }

    /// 一致している記録は信じ、**実物に聞かない**。ここが常に真になると、設定パネルを
    /// 開くたびに python.exe を起動することになる。
    #[test]
    fn a_matching_record_means_python_need_not_be_asked() {
        assert!(!should_ask_python(&current_pins()), "一致している記録は信じる");
        assert!(should_ask_python(&Default::default()), "記録が無ければ実物に聞く");
        let mut other = current_pins();
        other.insert("python".to_string(), "OLD".to_string());
        assert!(should_ask_python(&other), "記録が違えば実物に聞く");
    }

    /// 記録どおりなら空、違う pin だけを名指しする。
    #[test]
    fn outdated_list_names_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        let mut recorded = current_pins();
        assert!(
            outdated_list(dir.path(), &recorded, &current_requirements(), &current_models()).is_empty(),
            "全部一致なら空"
        );
        recorded.insert("irodori_tts".to_string(), "OLD".to_string());
        assert_eq!(
            outdated_list(dir.path(), &recorded, &current_requirements(), &current_models()),
            ["irodori_tts"]
        );
    }

    /// **`sidecar.py` の既定値が Rust の正本と食い違わないこと**
    /// （2026-09-11、開発方針 7 の掃討で追加）。
    ///
    /// モデルの正本は Rust（`MODEL_PINS`）で、`sidecar.py` の定数は
    /// **引数が渡されなかったとき用の保険**。ただし保険が古いままだと、渡し忘れた
    /// 経路だけ黙って別のモデルを読む。**正本が 2 つある形は、噛み合わせを見張って初めて安全。**
    #[test]
    fn the_sidecar_defaults_match_the_rust_pins() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("python")
            .join("sidecar.py");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("sidecar.py を読めない {}: {e}", path.display()));
        // **代入の行そのものを見る**（v0.5.7 の変異テストで発覚）。以前は「文字列がファイルのどこかに
        // ある」だけを見ており、v4 系の一覧（`V4_MODELS`）に同じ名前を書いたら、既定値を v3 に戻しても通った。
        for (name, repo, rev) in MODEL_PINS {
            let suffix = match *name {
                "model_synth" => "SYNTH",
                "model_voice_design" => "VOICE_DESIGN",
                "model_codec" => "CODEC",
                other => panic!("sidecar.py の定数の名前が分からない: {other}"),
            };
            let repo_line = format!("MODEL_REPO_{suffix} = \"{repo}\"");
            assert!(
                src.lines().any(|l| l == repo_line),
                "{name}: sidecar.py の既定値が Rust の正本と違う（{repo_line} が無い）"
            );
            let rev_line = format!("MODEL_REVISION_{suffix} = \"{rev}\"");
            assert!(
                src.lines().any(|l| l == rev_line),
                "{name}: sidecar.py の既定の revision が Rust の正本と違う（{rev_line} が無い）"
            );
        }
        assert!(
            src.contains("_apply_model_args"),
            "sidecar.py が Rust からの指定を受け取らなくなっている（正本が 2 つに割れる）"
        );
    }

    /// **合成モデルとコーデックの revision は固定値（40 桁の commit）**（v0.5.7 項目 2）。
    /// `main` は「そのとき最新」なので、上げても届かないし黙って変わる。コーデックは `main` の間、
    /// 上流が共有 HF キャッシュから読み、新規の人は最初の合成で約 0.43 GB を黙って取っていた。
    #[test]
    fn the_synth_and_codec_revisions_are_fixed_commits() {
        for want in ["model_synth", "model_voice_design", "model_codec"] {
            let (_, repo, rev) = MODEL_PINS.iter().find(|(n, _, _)| *n == want).unwrap();
            assert!(
                rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()),
                "{want} ({repo}) の revision が固定の commit ではない: {rev}"
            );
        }
    }

    /// **参照音声の生成は合成と同じモデル（同じ値）で、サイドカーは合成のランタイムを使い回す**（v0.5.7 項目 3）。
    /// 上流の `from_key` はキャッシュしないので、別々に作ると同じモデルを 2 つ読み込み VRAM を 2 倍使う。
    /// `sidecar.py` は単体で動かせない（fastapi などが要る）ので、本文のテキストで固定する。
    #[test]
    fn voice_design_shares_the_synth_model_and_runtime() {
        let get = |n: &str| MODEL_PINS.iter().find(|(name, _, _)| *name == n).map(|(_, r, v)| (*r, *v)).unwrap();
        assert_eq!(get("model_voice_design"), get("model_synth"), "生成と合成は同じ repo@revision");
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python").join("sidecar.py"),
        )
        .unwrap().replace("\r\n", "\n");
        let body = &src[src.find("    def _load_voice_design(self):").expect("_load_voice_design が無い")..];
        let body = &body[..body.find("\n    @staticmethod").unwrap()];
        let shared = body
            .find("if (MODEL_REPO_VOICE_DESIGN, MODEL_REVISION_VOICE_DESIGN) == (MODEL_REPO_SYNTH, MODEL_REVISION_SYNTH):")
            .expect("同じモデルかを見ていない");
        let reuse = body.find("return self._load_synth()").expect("合成のランタイムを使い回していない");
        let build = body.find("self._build_runtime(").expect("違うモデルのときの読み込みが無い");
        assert!(shared < reuse && reuse < build, "同じなら使い回し、違うときだけ別に読む順になっていない");
    }

    /// **いまのビルドの合成モデルは `sidecar.py` の v4 系の一覧に入っていて、v3 は入っていない**（v0.5.7 項目 2）。
    ///
    /// 精度（bf16）とサンプラー（既定 16 ステップ）は `sidecar.py` が読み込むモデルで決める。`MODEL_PINS` だけ
    /// 変えて一覧を直し忘れると、新しいモデルが fp32・sway 8 で動く。逆に v3 が入ると、更新前の人の v3 が
    /// ユーザーの聴いていない bf16・16 ステップに変わる（spec §6.0 項目 2・4。v3 は v0.5.6 の値のまま）。
    #[test]
    fn the_current_synth_model_is_the_only_v4_model_and_v3_is_not() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python").join("sidecar.py"),
        )
        .unwrap().replace("\r\n", "\n");
        let line = src
            .lines()
            .find(|l| l.starts_with("V4_MODELS = frozenset({"))
            .expect("sidecar.py に V4_MODELS が無い");
        let synth = MODEL_PINS.iter().find(|(n, _, _)| *n == "model_synth").unwrap().1;
        assert!(line.contains(&format!("\"{synth}\"")), "いまの合成モデルが v4 系の一覧に無い: {line}");
        let baseline_synth = v054_baseline_models()["model_synth"].clone();
        let v3 = baseline_synth.split('@').next().unwrap();
        assert!(!line.contains(&format!("\"{v3}\"")), "v3 が v4 系の一覧に入っている: {line}");
    }

    /// **モデルも更新の対象になる** (v0.5.5 項目 3)。
    ///
    /// v0.5.4 の経路はモデルを見ていなかった。`sidecar.py` は毎起動で上書きコピー
    /// されるのに重みは初回 DL でしか取らないので、**ID を変えると「コードだけ新しく
    /// なって重みが無い」**状態になり、`decide_fallback` が全部フォールバックさせて
    /// 高品質モードが無言で消える（「届かない」ではなく「静かに壊れる」）。
    #[test]
    fn changing_a_model_makes_it_outdated() {
        let dir = tempfile::tempdir().unwrap();
        place_current_model_files(dir.path());
        let mut models = current_models();
        assert!(
            outdated_list(dir.path(), &current_pins(), &current_requirements(), &models).is_empty(),
            "全部一致なら空"
        );

        // revision を上げた ＝ 既存環境へ届けなければならない
        let old = models.get("model_synth").cloned().unwrap();
        models.insert("model_synth".to_string(), format!("{old}-old"));
        let got = outdated_list(dir.path(), &current_pins(), &current_requirements(), &models);
        assert_eq!(got, ["model_synth"], "モデルの変更が対象に入らないと届かない");
    }

    /// **取得しに行く先は、ビルドが求める pin を全部渡すこと** (v0.5.5 項目 3 / v0.5.6 項目 3a)。
    ///
    /// 渡さない分は `sidecar.py` の既定値で取りに行き、Rust が求めているものと食い違う。
    /// **読む先は別**（記録から決める。`the_read_targets_come_from_the_record_name_by_name`）。
    #[test]
    fn model_args_cover_every_pin() {
        let args = model_args_for_fetch();
        for (_, repo, rev) in MODEL_PINS {
            assert!(args.iter().any(|a| a == repo), "{repo} を渡していない: {args:?}");
            assert!(args.iter().any(|a| a == rev), "{repo} の revision を渡していない");
        }
        // フラグと値が対になっていること
        assert_eq!(args.len(), MODEL_PINS.len() * 4, "フラグと値の対が崩れている: {args:?}");
        for flag in ["--model-synth", "--model-voice-design", "--model-codec"] {
            assert!(args.iter().any(|a| a == flag), "{flag} が無い");
            assert!(
                args.iter().any(|a| a == &format!("{flag}-revision")),
                "{flag}-revision が無い"
            );
        }
    }

    /// **入れ直せたモデルだけを記録する**（開発方針 7 —「対になる関数」に同じ観点を移植）。
    #[test]
    fn only_updated_models_are_recorded() {
        let updated = vec!["model_synth".to_string()];
        let got = merged_models(&Default::default(), &updated);
        assert_eq!(got.len(), 1, "入れ直した 1 本だけ: {got:?}");
        assert_eq!(got.get("model_synth"), current_models().get("model_synth"));
        assert!(!got.contains_key("model_codec"), "触っていないものを書かない");
    }

    /// **`current_requirements` と `recorded_distributions` は噛み合っていること**
    /// （2026-09-11、CLAUDE.md 開発方針 7 の掃討で発見）。
    ///
    /// 2 つは同じ定数（`COMMON_REQUIREMENTS` / `TORCH_PACKAGES` /
    /// `IRODORI_EXTRA_REQUIREMENTS`）から**別々に**作られる対で、片方に足して
    /// もう片方を忘れられる。忘れた側で起きることが違う:
    /// - `current_requirements` から漏れる → **更新判定に乗らず、永久に届かない**
    ///   （v0.5.5 が直した穴が 1 段上で再現する）
    /// - `recorded_distributions` から漏れる → 実際に入った版が記録されない
    #[test]
    fn the_two_requirement_lists_stay_in_sync() {
        let reqs = current_requirements();
        let recorded = recorded_distributions();
        // 固定 URL で入れる 3 本は `recorded_distributions` にしか無い（名前で入れないため）。
        let git_only = ["silentcipher", "dacvae", "irodori-tts"];

        for name in reqs.keys() {
            assert!(
                recorded.iter().any(|n| n == name),
                "{name} が recorded_distributions に無い（実際に入った版が記録されない）"
            );
        }
        for name in &recorded {
            if git_only.contains(&name.as_str()) {
                continue;
            }
            assert!(
                reqs.contains_key(name),
                "{name} が current_requirements に無い（更新判定に乗らず永久に届かない）"
            );
        }
    }

    /// **pin を増やしたら `current_pins` にも足す。** 足し忘れると、その依存だけ
    /// 更新判定から外れて「最新」と誤認する。
    #[test]
    fn pins_cover_every_pinned_url() {
        let pins = current_pins();
        assert_eq!(pins.len(), 4, "pin を増減したらここも更新する: {pins:?}");
        for url in pins.values() {
            assert!(url.starts_with("https://"), "URL でない: {url}");
        }
        assert!(pins.values().any(|u| u == IRODORI_TTS_ZIPBALL));
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;

    fn make_site(dir: &Path) -> PathBuf {
        let site = dir.join("python").join("Lib").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        site
    }

    fn put_pkg(site: &Path, pkg: &str, version: &str, body: &str) {
        std::fs::create_dir_all(site.join(pkg)).unwrap();
        std::fs::write(site.join(pkg).join("__init__.py"), body).unwrap();
        let di = site.join(format!("{pkg}-{version}.dist-info"));
        std::fs::create_dir_all(&di).unwrap();
        std::fs::write(di.join("METADATA"), format!("Version: {version}")).unwrap();
    }

    /// **Python 本体は入れ直しの対象にしない。**
    ///
    /// `ensure_python_embeddable` は `python.exe` があれば skip するので、
    /// pin を上げても反映されない。稼働中のインタプリタを安全に差し替える方法は
    /// 無いため、黙って部分更新して「最新」と記録するのではなく対象外にする。
    /// **実機検証用** (v0.5.4 項目 3、test-plan §5 E-7)。**実環境を書き換える**ので
    /// 既定では走らない。対象を環境変数で明示させ、うっかり実行できないようにしている。
    ///
    /// ```powershell
    /// $env:UGG_IRODORI_REAL_ROOT = "$env:APPDATA\\ugg\\irodori"
    /// cargo test -- --ignored --nocapture irodori_update_on_a_real_runtime
    /// ```
    ///
    /// 確かめるのは「入った」ではなく「**使える**」まで: `update_irodori_runtime` は
    /// import 確認を通ってから退避を捨てるので、成功して戻ればその時点で import できている。
    #[tokio::test]
    #[ignore = "実環境を書き換える。UGG_IRODORI_REAL_ROOT を指定して明示的に実行する"]
    async fn irodori_update_on_a_real_runtime() {
        let Ok(root) = std::env::var("UGG_IRODORI_REAL_ROOT") else {
            panic!("UGG_IRODORI_REAL_ROOT が未設定です（対象を明示すること）");
        };
        let root = PathBuf::from(root);
        assert!(
            root.join("python").join("python.exe").is_file(),
            "python.exe が無い: {}",
            root.display()
        );
        // v0.5.6 から更新の最後に `sidecar.py --synth-once` で確かめるので、対象の `sidecar.py` を
        // このリポジトリのものへ置き換える（ugg が起動のたびに行うのと同じ上書き）。
        crate::tts::sidecar::install_sidecar_script(Path::new(env!("CARGO_MANIFEST_DIR")), &root)
            .expect("sidecar.py を置けること");

        // **実行の証跡をファイルへ残す。** 実機検証の出力は端末の履歴と見分けがつかず、
        // 2026-09-11 に古い出力を新しい実行と取り違えて 2 往復を空費した。
        let log_path = std::env::temp_dir().join("ugg-e7-verify.log");
        let _ = std::fs::write(&log_path, "");
        let log = log_path.clone();
        let say = move |line: String| {
            use std::io::Write;
            println!("{line}");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(f, "{line}");
            }
        };
        say(format!(
            "[harness] E-7 rev2 差分ゲート / {} / log={}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            log_path.display()
        ));

        let before_imports = import_report(&root.join("python").join("python.exe"));
        say(format!("[before] imports={before_imports:?}"));
        let before = status(&root);
        say(format!(
            "[before] present={} has_record={} up_to_date={}",
            before.present, before.has_record, before.up_to_date
        ));
        say(format!("[before] outdated={:?}", before.outdated));
        say(format!("[before] python={:?}", installed_python_version(&root)));
        assert!(before.present, "前提: 使える状態であること");
        assert!(!before.up_to_date, "前提: 更新対象があること（既に最新なら検証にならない）");
        assert!(
            !before.outdated.iter().any(|n| n == "python"),
            "python が対象に混ざっている: {:?}",
            before.outdated
        );

        let outcome = update_irodori_runtime(&root, &before.outdated, |l| say(format!("  | {l}"))).await;
        let updated = match outcome {
            Ok(v) => {
                say(format!("[updated] {v:?}"));
                v
            }
            Err(e) => {
                // 失敗そのものは想定内。**失敗したときに何が残ったか**まで証跡に出す。
                say(format!("[failed] {e:#}"));
                say(format!(
                    "[failed] backup_left={} stamp_written={}",
                    root.join(UPDATE_BACKUP_DIR).exists(),
                    root.join(STAMP_FILE).exists()
                ));
                say(format!(
                    "[failed] imports={:?}",
                    import_report(&root.join("python").join("python.exe"))
                ));
                panic!("入れ直しに失敗: {e:#}");
            }
        };
        assert_eq!(updated, before.outdated, "対象が全部入れ直されること");
        assert!(
            !root.join(UPDATE_BACKUP_DIR).exists(),
            "成功したら退避を残さない: {}",
            root.join(UPDATE_BACKUP_DIR).display()
        );
        let after = import_report(&root.join("python").join("python.exe"));
        say(format!("[after] imports={after:?}"));
        let after_status = status(&root);
        say(format!("[after] status={after_status:?}"));
        say(format!(
            "[after] installed.json={}",
            std::fs::read_to_string(root.join(STAMP_FILE))
                .unwrap_or_else(|e| format!("(読めません: {e})"))
        ));
        for pkg in ["dacvae", "irodori_tts", "silentcipher"] {
            say(format!("[after] {pkg}: {}", describe_package(&site_of(&root), pkg)));
        }
        assert!(after_status.has_record, "記録が書かれていること");
        assert!(
            after_status.resolved.len() > 1,
            "実際に入った版を記録しきること（python だけでは記録の意味が無い）: {:?}",
            after_status.resolved
        );
        assert!(
            after_status.up_to_date,
            "入れ直しきったら最新になること: outdated={:?}",
            after_status.outdated
        );
        assert!(
            import_regressions(&before_imports, &after).is_empty(),
            "入れ直す前に使えていたものが使えなくなっている"
        );
    }

    /// 実環境のテストの対象（`UGG_IRODORI_REAL_ROOT` で明示させる。うっかり実行できないように）。
    fn real_root() -> PathBuf {
        let Ok(root) = std::env::var("UGG_IRODORI_REAL_ROOT") else {
            panic!("UGG_IRODORI_REAL_ROOT が未設定です（対象を明示すること）");
        };
        let root = PathBuf::from(root);
        assert!(
            root.join("python").join("python.exe").is_file(),
            "python.exe が無い: {}",
            root.display()
        );
        root
    }

    /// **実機検証用**（v0.5.6 項目 3b、test-plan E-10 の 4）。実物のランタイムで 1 回合成のゲートを走らせる。
    ///
    /// **環境は変えない**（`.update-gate\` に参照音声の写しを置き、終わったら消す。事前変換の結果もそこに作られる）。
    /// いまのビルドの値で合格すること（読み込みを含めた所要時間を出す — 締め切り 10 分の根拠になる実測）と、
    /// **重みの無い読み先を試すと「戻す」側に倒れる**ことを確かめる。ugg を終了してから走らせる（GPU を空ける）。
    ///
    /// 対象の `sidecar.py` は**このリポジトリのものへ置き換える**（ugg が起動のたびに行うのと同じ上書き）。
    /// 対象には最後に起動した ugg の `sidecar.py` があり、それが v0.5.5 以前だと `--synth-once` を知らない。
    ///
    /// ```powershell
    /// $env:UGG_IRODORI_REAL_ROOT = "$env:APPDATA\\ugg\\irodori"
    /// cargo test -- --ignored --nocapture irodori_gate_on_a_real_runtime
    /// ```
    #[test]
    #[ignore = "実物のランタイムで合成する。UGG_IRODORI_REAL_ROOT を指定して明示的に実行する"]
    fn irodori_gate_on_a_real_runtime() {
        let root = real_root();
        let py = root.join("python").join("python.exe");
        crate::tts::sidecar::install_sidecar_script(Path::new(env!("CARGO_MANIFEST_DIR")), &root)
            .expect("sidecar.py を置けること");
        let (read, from) = model_args_for_read(&root);
        println!("[gate] 読み先（{from}から）: {}", read.join(" "));
        println!("[gate] 材料: {:?}", pick_gate_voice_ref(&root));

        let fetch = model_args_for_fetch();
        let passed = run_synth_gate(&root, &py, &fetch, true, |l| println!("  | {l}"));
        println!("[gate] いまのビルドの値で: {passed:?} → {:?}", gate_verdict(&passed, None));
        assert!(
            matches!(passed, GateOutcome::Passed { .. }),
            "合格すること（参照音声が無い・GPU が見えない環境では確かめられない）: {passed:?}"
        );

        // 重みの無い読み先（存在しない revision）を試す
        let mut bogus = fetch.clone();
        let at = bogus
            .iter()
            .position(|a| a == "--model-synth-revision")
            .expect("revision の引数がある");
        bogus[at + 1] = "ugg-no-such-revision".to_string();
        let failed = run_synth_gate(&root, &py, &bogus, true, |l| println!("  | {l}"));
        println!("[gate] 重みの無い読み先で: {failed:?} → {:?}", gate_verdict(&failed, None));
        assert!(
            matches!(gate_verdict(&failed, None), GateVerdict::RollBack(_)),
            "重みが無ければ不合格として戻す側に倒れること: {failed:?}"
        );
        assert!(!root.join(GATE_DIR).exists(), "作業場所を残さない");
    }

    /// **実機検証用**（v0.5.7 の受け入れ条件 (b)、test-plan E-11 の A5）。v0.5.6 の実環境で、**途中で失敗した更新が
    /// transformers 5 → 4 まで全部戻り、v3 で喋れる**ことを確かめる。**実環境を書き換え、通信が要る**（入れ替えと戻しで
    /// pip が数百 MB を取る）。**ugg を終了してから**走らせる（生きているサイドカーが残っていれば始めない）。
    ///
    /// 失敗は、取得するモデルの revision を存在しないものに差し替えて起こす（テストのビルドだけの差し込み口）。
    /// 本物の `update_irodori_runtime` が、透かしの重みの先取り → 名前付き要件 → 固定 URL と入れ替えたあと、
    /// モデルの取得で落ちて全部戻す。v3 のモデルは消えない（片付けはゲートが両方通ったあとだけ）。
    ///
    /// ```powershell
    /// $env:UGG_IRODORI_REAL_ROOT = "$env:APPDATA\ugg\irodori"
    /// cargo test -- --ignored --nocapture irodori_failed_update_rolls_back_on_a_real_runtime
    /// ```
    #[tokio::test]
    #[ignore = "実環境を書き換える（通信が要る）。UGG_IRODORI_REAL_ROOT を指定して明示的に実行する"]
    async fn irodori_failed_update_rolls_back_on_a_real_runtime() {
        let root = real_root();
        let py = root.join("python").join("python.exe");
        let log_path = std::env::temp_dir().join("ugg-e11-rollback.log");
        let _ = std::fs::write(&log_path, "");
        let log = log_path.clone();
        let say = move |line: String| {
            use std::io::Write;
            println!("{line}");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(f, "{line}");
            }
        };
        say(format!(
            "[harness] E-11 (b) 途中で失敗した更新の全戻し / {} / log={}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            log_path.display()
        ));
        let live = crate::tts::sidecar::live_sidecars(&root, &reqwest::Client::new()).await;
        assert!(live.is_empty(), "ugg を終了してから走らせること（生きているサイドカー: {} 件）", live.len());
        let _busy = IrodoriBusyGuard::acquire_for(&root).expect("更新の錠を取れること（ugg を終了してから）");
        crate::tts::sidecar::install_sidecar_script(Path::new(env!("CARGO_MANIFEST_DIR")), &root)
            .expect("sidecar.py を置けること");

        let before = status(&root);
        say(format!("[before] outdated={:?}", before.outdated));
        assert!(before.present, "前提: 使える状態であること");
        for name in ["transformers", "model_synth"] {
            assert!(before.outdated.iter().any(|n| n == name), "前提: v0.5.6 の環境であること（{name} が対象に無い）");
        }
        let versions_before = query_all_versions(&py, |l| say(format!("  | {l}"))).expect("版を控えられること");
        let transformers_before = versions_before.get("transformers").cloned();
        say(format!("[before] transformers={transformers_before:?} 配布 {} 件", versions_before.len()));
        assert!(
            transformers_before.as_deref().is_some_and(|v| v.starts_with("4.")),
            "前提: transformers 4 の環境であること"
        );
        let stamp_before = std::fs::read(root.join(STAMP_FILE)).ok();
        let read_before = model_args_for_read(&root).0;
        let pinned_before: Vec<String> =
            ["irodori_tts", "dacvae", "silentcipher"].iter().map(|p| describe_package(&site_of(&root), p)).collect();
        let models_dir = |name: &str| root.join("model").join(name);
        let v3 = model_dir_of(&v054_baseline_models()["model_synth"]);
        assert!(models_dir(&v3).is_dir(), "前提: v3 のモデルがあること: {v3}");

        // 取得先を存在しない revision にする（名前付き要件と固定 URL を入れ替えたあと、モデルの取得で落ちる）
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                *FETCH_ARGS_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        let _reset = Reset;
        let mut bogus = model_args_from(&current_models());
        let at = bogus.iter().position(|a| a == "--model-synth-revision").expect("revision の引数がある");
        bogus[at + 1] = "ugg-no-such-revision".to_string();
        *FETCH_ARGS_FOR_TEST.lock().unwrap() = Some(bogus);

        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = lines.clone();
        let say2 = say.clone();
        let result = update_irodori_runtime(&root, &before.outdated, move |l| {
            seen.lock().unwrap().push(l.to_string());
            say2(format!("  | {l}"));
        })
        .await;
        drop(_reset);
        say(format!("[result] {:?}", result.as_ref().map_err(|e| format!("{e:#}"))));
        // 取得の前に sidecar.py が作る、存在しない revision の置き場所（このテストが作ったもの）だけを片付ける
        let synth_repo = current_models()["model_synth"].rsplit_once('@').map(|(r, _)| r.to_string()).unwrap();
        let junk = models_dir(&model_dir_name(&synth_repo, "ugg-no-such-revision"));
        if junk.is_dir() {
            let _ = std::fs::remove_dir_all(&junk);
            say(format!("[cleanup] テストが作った置き場所を消しました: {}", junk.display()));
        }
        assert!(result.is_err(), "取得で落ちて失敗として返ること");
        let lines = lines.lock().unwrap().clone();
        let prefetch = lines.iter().position(|l| l.contains("透かし（SilentCipher）の重みを取得しています"));
        let named = lines.iter().position(|l| l.contains("transformers"));
        say(format!("[progress] 透かしの先取り={prefetch:?} 名前付き要件={named:?}"));
        assert!(prefetch.is_some(), "透かしの重みを最初の段で取っていない");

        // 全部戻った
        let versions_after = query_all_versions(&py, |l| say(format!("  | {l}"))).expect("版を読めること");
        let not_restored = versions_to_restore(&versions_before, &versions_after);
        say(format!("[after] transformers={:?} 戻っていないもの={not_restored:?}", versions_after.get("transformers")));
        assert!(not_restored.is_empty(), "控えの版へ戻っていない: {not_restored:?}");
        assert_eq!(versions_after.get("transformers").cloned(), transformers_before, "transformers 4 まで戻ること");
        let added: Vec<&String> = versions_after.keys().filter(|k| !versions_before.contains_key(*k)).collect();
        say(format!("[after] 新しく入って残った配布={added:?}"));
        let pinned_after: Vec<String> =
            ["irodori_tts", "dacvae", "silentcipher"].iter().map(|p| describe_package(&site_of(&root), p)).collect();
        assert_eq!(pinned_after, pinned_before, "固定 URL の 3 本が戻ること");
        assert_eq!(std::fs::read(root.join(STAMP_FILE)).ok(), stamp_before, "記録を書き換えないこと");
        assert!(!root.join(UPDATE_BACKUP_DIR).exists(), "退避を残さないこと");
        assert!(!versions_snapshot_path(&root).exists(), "版の控えを残さないこと");
        assert_eq!(model_args_for_read(&root).0, read_before, "読み先が v3 のままであること");
        assert!(models_dir(&v3).is_dir(), "v3 のモデルを消さないこと");

        // 透かしの重みは先取りで共有キャッシュにある（v3 の合成で黙って取りに行かない）
        let hub = std::env::var_os("HF_HUB_CACHE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("hub")))
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".cache/huggingface/hub")
            });
        let weights = hub.join("models--sony--silentcipher").join("snapshots");
        say(format!("[after] 透かしの重み: {} ({})", weights.display(), weights.is_dir()));
        assert!(weights.is_dir(), "透かしの重みが共有キャッシュに無い（合成のときに黙って取りに行く）");

        // v3 で喋れる
        let outcome = run_synth_gate(&root, &py, &read_before, false, |l| say(format!("  | {l}")));
        say(format!("[v3] {outcome:?} → {:?}", gate_verdict(&outcome, None)));
        assert!(matches!(outcome, GateOutcome::Passed { .. }), "戻したあと v3 で合成できること: {outcome:?}");
        say("[harness] PASS".to_string());
    }

    /// **実機検証用**（test-plan E-11 の段階 2 の前）。更新が**プロセスごと中断して**版の控え（`update-versions.json`）が
    /// 残った実環境で、次の更新の冒頭の後始末（`recover_interrupted_update`）が**控えの版へ全部戻し**、控えを片付け、
    /// v3 で喋れることを確かめる（v0.5.6 項目 3d。E-11 の実施中に dev の再起動で更新が中断し、transformers 5 が入ったまま
    /// 記録は旧のままの環境ができた）。**実環境を書き換え、通信が要る。** ugg を終了してから走らせる。
    ///
    /// ```powershell
    /// $env:UGG_IRODORI_REAL_ROOT = "$env:APPDATA\ugg\irodori"
    /// cargo test -- --ignored --nocapture irodori_interrupted_update_recovers_on_a_real_runtime
    /// ```
    #[tokio::test]
    #[ignore = "実環境を書き換える（通信が要る）。UGG_IRODORI_REAL_ROOT を指定して明示的に実行する"]
    async fn irodori_interrupted_update_recovers_on_a_real_runtime() {
        let root = real_root();
        let py = root.join("python").join("python.exe");
        let live = crate::tts::sidecar::live_sidecars(&root, &reqwest::Client::new()).await;
        assert!(live.is_empty(), "ugg を終了してから走らせること（生きているサイドカー: {} 件）", live.len());
        let _busy = IrodoriBusyGuard::acquire_for(&root).expect("更新の錠を取れること（ugg を終了してから）");
        crate::tts::sidecar::install_sidecar_script(Path::new(env!("CARGO_MANIFEST_DIR")), &root)
            .expect("sidecar.py を置けること");

        let snapshot = read_versions_snapshot(&root).expect("前提: 中断した更新の版の控えが残っていること");
        let now = query_all_versions(&py, |l| println!("  | {l}")).expect("版を読めること");
        let differ = versions_to_restore(&snapshot, &now);
        println!("[before] 控えと違う配布={differ:?} 控えに無い配布={:?}", added_since(&snapshot, &now));
        assert!(!differ.is_empty(), "前提: 中断した更新で入れ替わった配布があること");
        let stamp_before = std::fs::read(root.join(STAMP_FILE)).ok();
        let read_before = model_args_for_read(&root).0;

        recover_interrupted_update(&root, &py, |l| println!("  | {l}")).expect("後始末が成功すること");

        let after = query_all_versions(&py, |l| println!("  | {l}")).expect("版を読めること");
        let left = versions_to_restore(&snapshot, &after);
        println!("[after] transformers={:?} 戻っていないもの={left:?}", after.get("transformers"));
        assert!(left.is_empty(), "控えの版へ戻っていない: {left:?}");
        assert!(read_versions_snapshot(&root).is_none(), "戻したら版の控えを片付けること");
        assert_eq!(std::fs::read(root.join(STAMP_FILE)).ok(), stamp_before, "記録を書き換えないこと");
        assert_eq!(model_args_for_read(&root).0, read_before, "読み先を変えないこと");

        let outcome = run_synth_gate(&root, &py, &read_before, false, |l| println!("  | {l}"));
        println!("[v3] {outcome:?}");
        assert!(matches!(outcome, GateOutcome::Passed { .. }), "戻したあと v3 で合成できること: {outcome:?}");
        println!("[harness] PASS");
    }

    /// **実機検証用**（v0.5.6 項目 3d、test-plan E-10 の 4）。名前付きの配布を**控えの版へ戻せる**ことを、
    /// 実物の pip で確かめる。**実環境を書き換え、通信が要る。** ugg を終了してから走らせる。
    ///
    /// 小さく、合成の経路が直接使わない `tqdm` を 1 つ別の版へ入れ替え、控えから戻す。入れ替え先は ugg の要件
    /// （`tqdm>=4.67.3`）を満たす版にするので、万一戻せなくても要件の範囲に収まる。
    ///
    /// ```powershell
    /// $env:UGG_IRODORI_REAL_ROOT = "$env:APPDATA\\ugg\\irodori"
    /// cargo test -- --ignored --nocapture irodori_rollback_on_a_real_runtime
    /// ```
    #[test]
    #[ignore = "実環境を書き換える（通信が要る）。UGG_IRODORI_REAL_ROOT を指定して明示的に実行する"]
    fn irodori_rollback_on_a_real_runtime() {
        let root = real_root();
        let py = root.join("python").join("python.exe");
        let before = query_all_versions(&py, |l| println!("  | {l}")).expect("版を控えられること");
        let tqdm = before.get("tqdm").expect("tqdm が入っていること").clone();
        let target = if tqdm == "4.67.3" { "4.68.0" } else { "4.67.3" };
        println!("[before] 配布 {} 件 / tqdm={tqdm} → {target} へ入れ替える", before.len());

        run_pip_install(&py, &["--no-deps", &format!("tqdm=={target}")], |l| println!("  | {l}"))
            .expect("入れ替えられること");
        let changed = query_all_versions(&py, |l| println!("  | {l}")).unwrap();
        println!("[changed] tqdm={:?}", changed.get("tqdm"));
        assert_eq!(
            versions_to_restore(&before, &changed),
            [("tqdm".to_string(), tqdm.clone())],
            "前提: tqdm だけが入れ替わったこと"
        );

        let left = roll_back_versions(&py, &before, |l| println!("  | {l}")).expect("戻せたか確かめられること");
        let after = query_all_versions(&py, |l| println!("  | {l}")).unwrap();
        println!("[after] tqdm={:?} 戻せなかったもの={left:?}", after.get("tqdm"));
        assert!(left.is_empty(), "戻せなかったもの: {left:?}");
        assert_eq!(after.get("tqdm"), Some(&tqdm), "控えの版へ戻ること");
        assert!(versions_to_restore(&before, &after).is_empty(), "ほかも控えのまま");
    }

    #[test]
    fn python_is_not_updatable_in_place() {
        assert!(updatable_pin("python").is_none());
        for name in ["silentcipher", "dacvae", "irodori_tts"] {
            assert!(updatable_pin(name).is_some(), "{name} は入れ直せるはず");
        }
        // current_pins の 4 件のうち、入れ直せるのは 3 件。
        let updatable = current_pins()
            .keys()
            .filter(|n| updatable_pin(n).is_some())
            .count();
        assert_eq!(updatable, 3, "pin を増やしたら updatable_pin にも足すこと");
    }

    /// 退避 → 復元で、**中身も dist-info も元どおりになる**。
    /// pip は「消してから入れる」ので、ここが戻らないと失敗時に環境が壊れる。
    fn report(pairs: &[(&str, Option<&str>)]) -> std::collections::BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(m, e)| (m.to_string(), e.map(|x| x.to_string())))
            .collect()
    }

    /// **元から壊れていたものを理由に巻き戻さない** (v0.5.4、実機検証で発覚)。
    ///
    /// 実機の `silentcipher` は `pydub` が無く一度も import できていない（upstream が
    /// 意図的に任意依存にしており、合成は透かし無しで成立する）。最初の実装は
    /// 「3 つとも import できること」を絶対条件にしていたため、**既存環境では必ず
    /// ロールバックし、更新が誰にも一度も成功しない**状態だった。
    #[test]
    fn only_a_regression_blocks_the_update() {
        let before = report(&[
            ("irodori_tts", None),
            ("dacvae", None),
            ("silentcipher", Some("ModuleNotFoundError: No module named 'pydub'")),
        ]);

        // 元から壊れていたものが壊れたままでも、それは入れ直しの失敗ではない
        assert!(
            import_regressions(&before, &before).is_empty(),
            "前から import できないものを理由に巻き戻してはいけない"
        );

        // 直っていたらなおよい（改善を失敗として数えない）
        let fixed = report(&[("irodori_tts", None), ("dacvae", None), ("silentcipher", None)]);
        assert!(import_regressions(&before, &fixed).is_empty());

        // 使えていたものが使えなくなったら、それは失敗。理由も添える
        let broken = report(&[
            ("irodori_tts", Some("ImportError: bad")),
            ("dacvae", None),
            ("silentcipher", Some("ModuleNotFoundError: No module named 'pydub'")),
        ]);
        let got = import_regressions(&before, &broken);
        assert_eq!(got.len(), 1, "壊れたのは 1 つ: {got:?}");
        assert!(got[0].starts_with("irodori_tts: "), "どれが壊れたか: {got:?}");
        assert!(got[0].contains("ImportError: bad"), "理由を落とさない: {got:?}");

        // 調べられなくなった（消えた）場合も悪化として扱う
        let gone = report(&[("dacvae", None), ("silentcipher", None)]);
        assert_eq!(import_regressions(&before, &gone), ["irodori_tts"]);
    }

    /// 見張る対象に、入れ直しうる 3 本が揃っていること。
    #[test]
    fn runtime_modules_cover_every_updatable_pin() {
        for name in current_pins().keys() {
            let Some((pkg, _)) = updatable_pin(name) else { continue };
            assert!(
                RUNTIME_MODULES.contains(&pkg),
                "{pkg} を入れ直すのに import の見張りが無い"
            );
        }
    }

    /// 退避が**途中まで**進んだ状態から戻せること（監査 ③）。
    ///
    /// 退避は「ディレクトリ」と「dist-info」の 2 段で、前者だけ動いて後者で失敗しうる。
    /// そのまま返すと site-packages からパッケージが消えたまま戻らない。
    #[test]
    fn a_half_done_aside_can_be_restored() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "dacvae", "1.0.0", "keep");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");
        // dist-info の移動先を中身つきで塞ぐ（Windows の rename はここで失敗する）
        std::fs::create_dir_all(backup.join("dacvae-1.0.0.dist-info").join("blocker")).unwrap();

        assert!(
            move_package_aside(&site, "dacvae", &backup).is_err(),
            "前提: 途中で失敗すること"
        );
        assert!(!site.join("dacvae").exists(), "前提: ディレクトリだけ先に動いている");

        restore_package(&site, &backup).expect("戻せること");
        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "keep",
            "消えたままにしない"
        );
    }

    /// **前回「守るために残した」退避を、次の実行が消してはいけない**（監査 ②）。
    ///
    /// 復元に失敗したときは退避を残してユーザーに場所を伝える。そこにあるのは
    /// **唯一残った旧版**なので、次の実行が冒頭で消すと、入れ直しに失敗した瞬間に
    /// 永久に失われる。残っていたらまず戻す。
    #[tokio::test]
    async fn a_leftover_backup_is_restored_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();

        // 前回の中断で退避だけが残っている状態を作る
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");
        std::fs::create_dir_all(backup.join("dacvae")).unwrap();
        std::fs::write(backup.join("dacvae").join("__init__.py"), b"old").unwrap();
        std::fs::create_dir_all(backup.join("dacvae-1.0.0.dist-info")).unwrap();
        assert!(!site.join("dacvae").exists(), "前提: site からは消えている");

        update_irodori_runtime(dir.path(), &[], |_| {})
            .await
            .expect("入れ直す対象が無くても、戻しは行われること");

        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "old",
            "退避していた旧版が site へ戻ること"
        );
        assert!(site.join("dacvae-1.0.0.dist-info").is_dir(), "dist-info も戻ること");
        assert!(
            !dir.path().join(UPDATE_BACKUP_DIR).exists(),
            "戻せたら退避は捨てる"
        );
    }

    fn map(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// 配布名の書き方の揺れを揃える（`importlib.metadata` の名前と要件の名前を突き合わせるため）。
    #[test]
    fn distribution_names_are_normalized() {
        assert_eq!(normalize_dist_name("Huggingface_Hub"), "huggingface-hub");
        assert_eq!(normalize_dist_name("irodori_tts"), "irodori-tts");
        assert_eq!(normalize_dist_name("ruamel.yaml"), "ruamel-yaml");
        assert_eq!(normalize_dist_name("a.-_b"), "a-b");
        assert_eq!(normalize_dist_name("torch"), "torch");
    }

    /// 全配布の版の行を読む（名前は正規化する）。無関係な行は拾わない。
    #[test]
    fn the_all_versions_line_is_parsed_with_normalized_names() {
        let line = format!(
            "{ALL_VERSIONS_MARKER}[[\"Huggingface_Hub\",\"0.36.2\",\"huggingface_hub-0.36.2.dist-info\"],\
             [\"torch\",\"2.10.0+cu128\",\"torch-2.10.0+cu128.dist-info\"]]"
        );
        let got = parse_all_versions_line(&line).expect("読めること");
        assert_eq!(got.get("huggingface-hub").map(String::as_str), Some("0.36.2"));
        assert_eq!(got.get("torch").map(String::as_str), Some("2.10.0+cu128"));
        assert!(parse_all_versions_line("Collecting torch").is_none());
        // 記録用の目印とは取り違えない
        assert!(parse_all_versions_line(&format!("{VERSIONS_MARKER}{{}}")).is_none());
    }

    /// **戻すのは「版が変わった・消えた」配布だけ**（v0.5.6 項目 3d）。
    /// 固定 URL の 3 本は含めない（PyPI には無いので版指定で入れ直せない。退避から戻す）。
    /// 新しく増えた配布も含めない（`added_since` でログにだけ出す）。
    #[test]
    fn only_changed_or_missing_distributions_are_restored() {
        let before = map(&[
            ("transformers", "4.57.6"),
            ("tokenizers", "0.22.1"),
            ("numpy", "1.26.4"),
            ("dacvae", "0.1.0"),
            ("irodori-tts", "0.1.0"),
        ]);
        let after = map(&[
            ("transformers", "5.0.0"),
            // tokenizers は消えた
            ("numpy", "1.26.4"),
            ("dacvae", "0.2.0"),
            ("irodori-tts", "0.2.0"),
            ("hf-xet", "1.0.0"),
        ]);
        assert_eq!(
            versions_to_restore(&before, &after),
            [
                ("tokenizers".to_string(), "0.22.1".to_string()),
                ("transformers".to_string(), "4.57.6".to_string()),
            ],
            "固定 URL の 3 本・変わっていないもの・増えたものは含めない"
        );
        assert_eq!(added_since(&before, &after), ["hf-xet"]);
    }

    /// 固定 URL の 3 本の名前は、正規化した名前で持つ（控えの名前は正規化済み）。
    #[test]
    fn the_pinned_distributions_are_the_updatable_pins() {
        for name in PIN_ORDER {
            assert!(updatable_pin(name).is_some(), "{name} は固定 URL の 1 本");
            assert!(
                PINNED_DISTRIBUTIONS.contains(&normalize_dist_name(name).as_str()),
                "{name} を版指定の入れ直しから外していない"
            );
        }
        assert_eq!(PINNED_DISTRIBUTIONS.len(), PIN_ORDER.len());
    }

    /// **戻しの pip も torch 系とそれ以外で分ける**（1 回にまとめると `--index-url` が全体に効き、
    /// CUDA の index に無いパッケージが戻せない）。
    #[test]
    fn a_rollback_splits_torch_from_the_rest() {
        let (torch, other) = split_by_index(&[
            ("torch".to_string(), "2.10.0+cu128".to_string()),
            ("transformers".to_string(), "4.57.6".to_string()),
            ("torchaudio".to_string(), "2.10.0+cu128".to_string()),
        ]);
        assert_eq!(torch, ["torch==2.10.0+cu128", "torchaudio==2.10.0+cu128"]);
        assert_eq!(other, ["transformers==4.57.6"]);
    }

    /// **CUDA 版の torch が CPU 版へ入れ替わったら失敗にする**（v0.5.6 項目 3c の守り）。
    /// 元から CPU 版の環境は咎めない（GPU の無い環境の更新を巻き戻さない）。
    #[test]
    fn a_cuda_torch_replaced_by_the_cpu_build_is_caught() {
        let cuda = map(&[("torch", "2.10.0+cu128")]);
        assert!(cuda_torch_replaced(&cuda, &map(&[("torch", "2.10.0")])).is_some(), "CPU 版へ");
        assert!(cuda_torch_replaced(&cuda, &map(&[])).is_some(), "消えた");
        assert!(cuda_torch_replaced(&cuda, &map(&[("torch", "2.10.1+cu128")])).is_none());
        let cpu = map(&[("torch", "2.10.0")]);
        assert!(cuda_torch_replaced(&cpu, &map(&[("torch", "2.10.0")])).is_none(), "元から CPU 版");
    }

    /// **段取りは固定順**（v0.5.6 項目 3c）。`outdated` のアルファベット順に従わない。
    #[test]
    fn the_update_plan_has_a_fixed_order() {
        let outdated: Vec<String> = [
            "irodori_tts",
            "model_synth",
            "silentcipher",
            "torch",
            "transformers",
            "dacvae",
            "model_codec",
            "no_such_thing",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let plan = update_plan(&outdated);
        assert_eq!(plan.pins, ["silentcipher", "dacvae", "irodori_tts"], "依存の順");
        assert_eq!(plan.torch.len(), 1, "torch は CUDA の index 側: {plan:?}");
        assert!(plan.torch[0].starts_with("torch"), "{plan:?}");
        assert_eq!(plan.other.len(), 1, "transformers はその他の側: {plan:?}");
        assert!(plan.other[0].starts_with("transformers"), "{plan:?}");
        assert!(plan.models, "モデルは 1 回にまとめて確かめる");
        assert_eq!(plan.skipped, ["no_such_thing"]);
        assert_eq!(plan.names.len(), 7, "入れ直すものは全部記録に回る: {plan:?}");
        assert!(plan.touches_packages());
        assert!(!update_plan(&["model_synth".to_string()]).touches_packages(), "モデルだけなら版の控えは要らない");
    }

    /// その他の要件を入れるとき、torch をいまの版に縛る（PyPI の CPU 版を取りに行かせない）。
    #[test]
    fn other_requirements_are_installed_with_torch_pinned() {
        let got = torch_constraints(&map(&[
            ("torch", "2.10.0+cu128"),
            ("torchaudio", "2.10.0+cu128"),
            ("transformers", "4.57.6"),
        ]));
        assert_eq!(got, ["torch==2.10.0+cu128", "torchaudio==2.10.0+cu128"]);
        assert!(torch_constraints(&map(&[("numpy", "1.26.4")])).is_empty());
    }

    /// **退避が済んだ配布を戻すときは、入れ直しで入った新しいものを先に退ける**（v0.5.6 項目 3d）。
    /// 退けないと dist-info が 2 つ残り、pip も import も新旧を取り違える。
    #[test]
    fn restoring_a_finished_aside_removes_the_new_install_first() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        let backup_root = dir.path().join(UPDATE_BACKUP_DIR);
        put_pkg(&site, "dacvae", "0.1.0", "old");
        move_package_aside(&site, "dacvae", &backup_root.join("dacvae")).unwrap();
        std::fs::write(aside_complete_marker(&backup_root, "dacvae"), b"").unwrap();
        // 入れ直しで新しい版が入った
        put_pkg(&site, "dacvae", "0.2.0", "new");

        restore_all_backups(&site, &backup_root).unwrap();

        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "old"
        );
        assert!(site.join("dacvae-0.1.0.dist-info").is_dir(), "旧版の dist-info が戻る");
        assert!(!site.join("dacvae-0.2.0.dist-info").exists(), "新しい dist-info を残さない");
    }

    /// **退避が途中で止まった配布は、site に残っている原本を消さない**（v0.5.6 項目 3d）。
    /// 退避の印が無い ＝ 入れ直しはまだ走っていない ＝ site に残っているのは動かせなかった原本。
    #[test]
    fn restoring_an_unfinished_aside_keeps_the_original_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        let backup_root = dir.path().join(UPDATE_BACKUP_DIR);
        put_pkg(&site, "dacvae", "0.1.0", "old");
        // ディレクトリだけ退避できて、dist-info は動かせなかった
        std::fs::create_dir_all(backup_root.join("dacvae")).unwrap();
        std::fs::rename(site.join("dacvae"), backup_root.join("dacvae").join("dacvae")).unwrap();

        restore_all_backups(&site, &backup_root).unwrap();

        assert!(site.join("dacvae-0.1.0.dist-info").is_dir(), "原本の dist-info を消していない");
        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "old"
        );
    }

    /// **退避のディレクトリにファイルがあっても、更新が止まらない**（反証 #0 の形）。
    /// 以前はファイルを退避として `restore_package` に渡し、読み取りに失敗して「戻せない」と止まっていた。
    /// 版の控えをここに置くと、以後ずっと更新できなくなるところだった（控えは外に置いた）。
    #[test]
    fn files_in_the_backup_dir_are_not_mistaken_for_backups() {
        let dir = tempfile::tempdir().unwrap();
        let _site = make_site(dir.path());
        let backup_root = dir.path().join(UPDATE_BACKUP_DIR);
        std::fs::create_dir_all(&backup_root).unwrap();
        std::fs::write(backup_root.join("dacvae.aside-complete"), b"").unwrap();
        std::fs::write(backup_root.join("versions.json"), b"{}").unwrap();
        let py = dir.path().join("python").join("python.exe");

        recover_interrupted_update(dir.path(), &py, |_| {}).expect("ファイルは退避として扱わない");
        assert!(!backup_root.exists(), "戻せたら退避を捨てる");
        assert!(
            !versions_snapshot_path(dir.path()).starts_with(&backup_root),
            "版の控えは退避のディレクトリの外に置く"
        );
    }

    /// **前回の更新の控えが残っていたら、黙って捨てない**（v0.5.6 項目 3d）。
    /// 戻せたか確かめられないとき（ここでは python が動かない）は、控えを残して止まる。
    #[tokio::test]
    async fn a_leftover_version_snapshot_is_not_dropped_silently() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();
        write_versions_snapshot(dir.path(), &map(&[("transformers", "4.57.6")])).unwrap();
        put_pkg(&site, "~ransformers", "4.57.6", "old");

        let err = update_irodori_runtime(dir.path(), &[], |_| {})
            .await
            .expect_err("戻せたか確かめられないうちは進まない");
        assert!(format!("{err:#}").contains("前回の更新"), "{err:#}");
        assert!(
            read_versions_snapshot(dir.path()).is_some(),
            "控えを残す（次の更新でもう一度戻す）"
        );
        assert!(site.join("~ransformers").is_dir(), "戻せていないうちは pip の一時退避を消さない");
    }

    /// **pip の一時退避（`~` で始まる dist-info）は、入っている版として数えない**（v0.5.7 の実機検証 E-11）。
    /// 入れ替えの途中で止まった pip は `~ransformers-4.57.6.dist-info` を残し、`distributions()` はこれも
    /// `transformers` として返す。名前で畳むと並び次第で古い版が残り、5.17.0 が入っているのに「4.57.6 のまま」と
    /// 読んで戻さなかった。どちらの並びでも、本物の dist-info の版だけを読む。
    #[test]
    fn a_pip_stash_is_not_counted_as_installed() {
        for (first, second) in [
            ("[\"transformers\",\"5.17.0\",\"transformers-5.17.0.dist-info\"]", "[\"transformers\",\"4.57.6\",\"~ransformers-4.57.6.dist-info\"]"),
            ("[\"transformers\",\"4.57.6\",\"~ransformers-4.57.6.dist-info\"]", "[\"transformers\",\"5.17.0\",\"transformers-5.17.0.dist-info\"]"),
        ] {
            let line = format!("{ALL_VERSIONS_MARKER}[{first},{second}]");
            let got = parse_all_versions_line(&line).expect("読めること");
            assert_eq!(got.get("transformers").map(String::as_str), Some("5.17.0"), "{line}");
            let before = map(&[("transformers", "4.57.6")]);
            assert_eq!(
                versions_to_restore(&before, &got),
                vec![("transformers".to_string(), "4.57.6".to_string())],
                "控えの版へ戻す対象に入る"
            );
        }
        // 一時退避しか無い（新しい版の dist-info が書かれる前に止まった）なら、入っていないと読む → 戻す対象になる
        let only_stash =
            format!("{ALL_VERSIONS_MARKER}[[\"transformers\",\"4.57.6\",\"~ransformers-4.57.6.dist-info\"]]");
        assert!(parse_all_versions_line(&only_stash).unwrap().get("transformers").is_none());
    }

    /// 後始末は、**控えの版へ全部戻せたと確かめたあと**、控えを捨てる前に pip の一時退避を片付ける
    /// （成功の経路は python が要るので、配線を本文のテキストで固定する。戻せなかったときに残すことは
    /// `a_leftover_version_snapshot_is_not_dropped_silently` が振る舞いで見る）。
    #[test]
    fn the_recovery_removes_pip_stashes_only_after_everything_is_restored() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("fn recover_interrupted_update<F>(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let returned = body.find("if !left.is_empty() {").expect("戻せなかったら止まる");
        let stash = body.find("let stash = remove_pip_stash(&site);").expect("一時退避を片付けていない");
        let dropped = body.find("remove_versions_snapshot(asset_root);").expect("控えを捨てる");
        assert!(returned < stash && stash < dropped, "戻せたと確かめたあと、控えを捨てる前に片付ける");
        assert_eq!(body.matches("remove_pip_stash(").count(), 1);
    }

    /// **pip の一時退避だけを消す**（v0.5.7 の実機検証 E-11）。本物のパッケージと dist-info、`~` を途中に含む名前は残す。
    #[test]
    fn only_pip_stashes_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "transformers", "5.17.0", "new");
        put_pkg(&site, "~ransformers", "4.57.6", "old");
        std::fs::write(site.join("~stray.pth"), b"").unwrap();
        std::fs::create_dir_all(site.join("a~b")).unwrap();

        let removed = remove_pip_stash(&site);
        assert_eq!(removed, vec!["~ransformers", "~ransformers-4.57.6.dist-info", "~stray.pth"]);
        assert!(site.join("transformers").is_dir());
        assert!(site.join("transformers-5.17.0.dist-info").is_dir());
        assert!(site.join("a~b").is_dir(), "先頭が `~` でないものは消さない");
        assert!(remove_pip_stash(&site).is_empty(), "2 回目は何もしない");
    }

    /// **後の段で失敗しても、前の段で入れ替えた分まで戻す**（v0.5.6 項目 3d。spec §6.0 の土台の欠落 ①）。
    ///
    /// 以前は、後の段（例: モデルの取得）で失敗すると退避を丸ごと消していたので、それより前に
    /// 入れ直しが成功した固定 URL の配布は**新しい版のまま、唯一の旧版を失っていた**。
    #[test]
    fn a_later_failure_rolls_back_what_earlier_steps_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        let backup_root = dir.path().join(UPDATE_BACKUP_DIR);
        for pkg in ["silentcipher", "dacvae"] {
            put_pkg(&site, pkg, "0.1.0", "old");
            move_package_aside(&site, pkg, &backup_root.join(pkg)).unwrap();
            std::fs::write(aside_complete_marker(&backup_root, pkg), b"").unwrap();
            put_pkg(&site, pkg, "0.2.0", "new");
        }
        let py = dir.path().join("python").join("python.exe");

        let err = roll_back_update(dir.path(), &py, None, anyhow!("HF モデルの取得に失敗しました"), |_| {});

        for pkg in ["silentcipher", "dacvae"] {
            assert_eq!(
                std::fs::read_to_string(site.join(pkg).join("__init__.py")).unwrap(),
                "old",
                "{pkg} を旧版へ戻す"
            );
            assert!(!site.join(format!("{pkg}-0.2.0.dist-info")).exists());
        }
        assert!(!backup_root.exists(), "戻せたら退避を捨てる");
        let message = format!("{err:#}");
        assert!(message.contains("HF モデルの取得に失敗しました"), "元の理由を残す: {message}");
        assert!(message.contains("元の版へ戻しました"), "戻したことを言う: {message}");
    }

    /// 版の控えは差し替えで書き、書きかけを残さない。
    #[test]
    fn the_version_snapshot_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let versions = map(&[("torch", "2.10.0+cu128"), ("numpy", "1.26.4")]);
        write_versions_snapshot(dir.path(), &versions).unwrap();
        assert_eq!(read_versions_snapshot(dir.path()), Some(versions));
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [VERSIONS_SNAPSHOT_FILE]);
        remove_versions_snapshot(dir.path());
        assert!(read_versions_snapshot(dir.path()).is_none());
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap()
    }

    /// **取得量の数字は 1 か所（ここの定数）で決め、画面の確認の文言と取説は同じ文字列を書く**（v0.5.7 項目 8）。
    /// 以前は 3 か所で「約 2〜3 GB」「約 2〜4 GB」「合計 10 GB 前後」と食い違っていた。
    #[test]
    fn download_sizes_agree_between_code_screen_and_manual() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let ts = std::fs::read_to_string(root.join("src/panels/settings.ts")).unwrap().replace("\r\n", "\n");
        let manual = std::fs::read_to_string(root.join("docs/manual.md")).unwrap().replace("\r\n", "\n");
        let html = std::fs::read_to_string(root.join("index.html")).unwrap().replace("\r\n", "\n");
        assert!(
            html.contains(&format!("ダウンロードには{FIRST_INSTALL_DOWNLOAD} の通信（入れたあとはディスクを{FIRST_INSTALL_DISK} 使います）")),
            "設定パネルの注記の数字が正本と違う"
        );
        for (name, value) in [
            ("IRODORI_FIRST_DOWNLOAD", FIRST_INSTALL_DOWNLOAD),
            ("IRODORI_FIRST_DISK", FIRST_INSTALL_DISK),
            ("IRODORI_MODEL_UPDATE_DOWNLOAD", MODEL_UPDATE_DOWNLOAD),
        ] {
            assert!(ts.contains(&format!("const {name} = \"{value}\";")), "settings.ts の {name} が {value} でない");
            assert!(manual.contains(value), "取説に {value} が無い（{name}）");
        }
        for stale in ["約 2〜3 GB", "約 2〜4GB", "約 2〜4 GB", "10GB 前後", "10 GB 前後", "1〜2 GB の通信", "1〜2GB"] {
            assert!(
                !ts.contains(stale) && !manual.contains(stale) && !html.contains(stale),
                "古い数字が残っている: {stale}"
            );
        }
        let torch = &rs_install_torch_body();
        assert!(torch.contains("{TORCH_DOWNLOAD}"), "PyTorch の取得の文言に正本の数字を使っていない");
        let rs = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs")).unwrap().replace("\r\n", "\n");
        let body = &rs[rs.find("pub async fn install_irodori_models<F>(").unwrap()..];
        assert!(body.contains("{MODEL_DOWNLOAD}"), "モデルの取得の進捗に正本の数字を使っていない");
    }

    fn rs_install_torch_body() -> String {
        let rs = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &rs[rs.find("pub async fn install_torch_cuda<F>(").unwrap()..];
        body[..body.find("\n}\n").unwrap()].to_string()
    }

    /// **更新に要る空き**（v0.5.7 項目 8）: モデルが変わると約 3.6 GB、PyTorch が変わると約 3 GB の取得に余裕を足す。
    #[test]
    fn the_free_space_needed_follows_what_the_update_fetches() {
        let plan = |names: &[&str]| update_plan(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(required_free_for_update(&plan(&["transformers"])), GIB);
        assert_eq!(required_free_for_update(&plan(&["model_synth"])), 6 * GIB);
        assert_eq!(required_free_for_update(&plan(&["torch"])), 5 * GIB);
        assert_eq!(required_free_for_update(&plan(&["torch", "model_synth"])), 10 * GIB);
    }

    /// 空きが足りなければ要る量を伝えて止める。**空きが分からなければ止めない**（分からないことを理由に断らない）。
    #[test]
    fn a_lack_of_free_space_stops_before_anything_changes() {
        assert_eq!(lacking_free_space(6 * GIB, None), None);
        assert_eq!(lacking_free_space(6 * GIB, Some(6 * GIB)), None);
        let why = lacking_free_space(6 * GIB, Some(2 * GIB)).expect("足りない");
        assert!(why.contains("6.0 GB") && why.contains("2.0 GB") && why.contains("4.0 GB"), "{why}");
        assert!(why.contains("何も変えていません"), "{why}");
        // 実物の API で空きが読める（無いフォルダは、あるところまで遡る）
        let dir = tempfile::tempdir().unwrap();
        assert!(free_bytes(&dir.path().join("まだ無い").join("フォルダ")).is_some_and(|b| b > 0));
    }

    /// 更新は**何も変える前**に、初回導入は Python を置く前に空きを確かめる（配線を本文のテキストで固定する）。
    #[test]
    fn the_free_space_is_checked_before_anything_changes() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("pub async fn update_irodori_runtime<F>(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let check = body
            .find("lacking_free_space(required_free_for_update(&plan), free_bytes(asset_root))")
            .expect("更新で空きを確かめていない");
        for later in ["import_report(&py_exe)", "write_versions_snapshot(", "apply_update_plan("] {
            let at = body.find(later).unwrap_or_else(|| panic!("{later} が無い"));
            assert!(check < at, "空きを {later} より後で確かめている");
        }
        let cmd = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &cmd[cmd.find("pub async fn download_irodori_assets").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let check = body.find("check_free_space_for_first_install(&asset_root)").expect("初回導入で空きを確かめていない");
        let python = body.find("ensure_python_embeddable(").unwrap();
        assert!(check < python, "Python を置いてから空きを確かめている");
    }

    /// **huggingface_hub の取得の進捗を、画面向けに直す**（v0.5.7 項目 8）。行は spike（hub 1.33・hf-xet）で実際に出たもの。
    #[test]
    fn hub_download_progress_is_turned_into_readable_lines() {
        let mut hf = HfProgress::default();
        // 大きなファイル: 組み立ての行（全体と割合）が出たら、それを優先する
        assert_eq!(
            hf.describe("model.safetensors: reconstructing file:   0%|          |  0.00B / 3.06GB            \x1b[A"),
            Some(Some("  取得中 model.safetensors 0 MB / 3.06 GB（0%）".to_string()))
        );
        assert_eq!(
            hf.describe("model.safetensors: reconstructing file:   0%|          | 89.4kB / 3.06GB            \x1b[A"),
            Some(None),
            "割合が変わらなければ出さない"
        );
        assert_eq!(
            hf.describe("model.safetensors: reconstructing file:  41%|████      | 1.26GB / 3.06GB            \x1b[A"),
            Some(Some("  取得中 model.safetensors 1.26 GB / 3.06 GB（41%）".to_string()))
        );
        assert_eq!(
            hf.describe("model.safetensors: downloading bytes: ███       |  956MB, 24.6MB/s  "),
            Some(None),
            "割合の行があるファイルの量だけの行は出さない"
        );
        // 量だけの行しか無いファイル: 50 MB 進むごと
        let mut only = HfProgress::default();
        assert_eq!(
            only.describe("weights.pth: downloading bytes:           |  154kB, 4.29kB/s  "),
            Some(Some("  取得中 weights.pth 0 MB（4.29kB/s）".to_string()))
        );
        assert_eq!(only.describe("weights.pth: downloading bytes: █  | 30.0MB, 20.1MB/s  "), Some(None));
        assert_eq!(
            only.describe("weights.pth: downloading bytes: ██ | 60.2MB, 21.0MB/s  "),
            Some(Some("  取得中 weights.pth 60 MB（21.0MB/s）".to_string()))
        );
        // 小さなファイル: いつもの tqdm
        let mut small = HfProgress::default();
        assert_eq!(
            small.describe("tokenizer.json: 100%|██████████| 6.72M/6.72M [00:00<00:00, 51.9MB/s]"),
            Some(Some("  取得中 tokenizer.json 7 MB / 7 MB（100%）".to_string()))
        );
        // 進捗でない行はそのまま流す
        assert_eq!(small.describe("Warning: You are sending unauthenticated requests to the HF Hub."), None);
        assert_eq!(small.describe("[hf-download] Aratako/X を確認中…"), None);
        assert_eq!(small.describe("Progress 10 of 100"), None);
    }

    /// 進捗の変換は、Python の出力を流す処理で pip の変換の次に通す（配線を本文のテキストで固定する）。
    #[test]
    fn the_hub_progress_converter_is_wired_into_python_output() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("fn run_python_lines<F>(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("let mut hf = HfProgress::default();"), "hub の進捗の変換を作っていない");
        assert!(
            body.contains("progress.describe(line.text).or_else(|| hf.describe(line.text))"),
            "hub の進捗の変換を通していない"
        );
    }

    #[test]
    fn progress_sizes_are_read_in_both_notations() {
        assert_eq!(parse_progress_size("3.06GB"), Some(3.06e9));
        assert_eq!(parse_progress_size("6.72M"), Some(6.72e6));
        assert_eq!(parse_progress_size("89.4kB"), Some(89.4e3));
        assert_eq!(parse_progress_size("668"), Some(668.0));
        assert_eq!(parse_progress_size("0.00B"), Some(0.0));
        assert_eq!(parse_progress_size("abc"), None);
    }

    /// **合格の文面に透かしの状態を出す**（v0.5.7 項目 5）。効いていなくても合格は合格（透かしが無くても合成は
    /// できる）。v0.5.4 は、一度も効いていないことに実機検証で初めて気づいた。
    #[test]
    fn the_gate_says_whether_the_watermark_works() {
        let say = |w: Option<bool>| match gate_verdict(&GateOutcome::Passed { ms: 7900, watermark: w, voice_design: false }, Some(true)) {
            GateVerdict::Pass(m) => m,
            other => panic!("合格にならない: {other:?}"),
        };
        assert!(say(Some(true)).contains("透かし: 効いています"), "{}", say(Some(true)));
        assert!(say(Some(false)).contains("透かし: 効いていません"), "{}", say(Some(false)));
        assert!(!say(None).contains("透かし"), "分からないときは言わない: {}", say(None));
        assert!(say(Some(false)).contains("7.9 秒"));
    }

    /// **透かしの重みは更新の最初の段（名前付き要件より前）で取り、失敗しても止めない**（v0.5.7 項目 5）。
    /// 途中で全戻ししても pydub は残るので、後ろに置くと残った pydub が合成のときに黙って取りに行かせる。
    #[test]
    fn the_watermark_weights_are_fetched_first_and_never_stop_the_update() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("async fn apply_update_plan<F>(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let fetch = body
            .find("    if let Err(err) = prefetch_watermark_weights(asset_root, |l| on_line(l)) {\n        report_watermark_prefetch_failure(")
            .expect("更新で透かしの重みを取っていない（または失敗で止まる形）");
        for later in ["if !plan.torch.is_empty()", "if !plan.other.is_empty()", "for pkg in &plan.pins", "if plan.models"] {
            let at = body.find(later).unwrap_or_else(|| panic!("{later} が無い"));
            assert!(fetch < at, "透かしの重みを {later} より後で取っている");
        }
        // 初回導入: huggingface_hub を入れた後、pydub を含む追加の依存より前。失敗しても止めない
        let cmd = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &cmd[cmd.find("pub async fn download_irodori_assets").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let common = body.find("install_common_requirements(").unwrap();
        let fetch = body
            .find("if let Err(err) = irodori_download::prefetch_watermark_weights(&asset_root, &emit) {\n            irodori_download::report_watermark_prefetch_failure(")
            .expect("初回導入で透かしの重みを取っていない（または失敗で止まる形）");
        let runtime = body.find("install_irodori_runtime(").unwrap();
        assert!(common < fetch && fetch < runtime, "初回導入の順が違う");
    }

    /// **`sidecar.py` は透かしの重みを上流と同じ呼び方で取る**（v0.5.7 項目 5）。上流の `watermark.py` →
    /// `silentcipher.get_model()` は `snapshot_download(repo_id="sony/silentcipher")`（revision も置き場所も
    /// 指定しない）で取りに行く。違う形で取ると上流が見る場所に入らず、合成のときにまた取りに行く。
    #[test]
    fn the_sidecar_fetches_the_watermark_weights_the_way_upstream_reads_them() {
        let py = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("python").join("sidecar.py"))
            .unwrap().replace("\r\n", "\n");
        for needle in [
            "WATERMARK_REPO = \"sony/silentcipher\"",
            "    snapshot_download(repo_id=WATERMARK_REPO)\n",
            "        \"--fetch-watermark\",",
            "    if args.fetch_watermark:",
            "\"watermark\": backend.watermark_ready()",
            "        _diag(f\"[irodori] 透かし: {said}\")",
        ] {
            assert!(py.contains(needle), "sidecar.py に無い: {needle}");
        }
        // 透かしの取得モードは HTTP を立てる前（--download-only と同じく）に分岐する
        let fetch = py.find("    if args.fetch_watermark:").unwrap();
        let download = py.find("    if args.download_only:").unwrap();
        assert!(fetch < download);
    }

    /// 子プロセスの終わり方と目印の行から、ゲートの結果を決める（v0.5.6 項目 3b）。
    #[test]
    fn the_gate_outcome_is_read_from_the_markers_and_the_exit() {
        let start = json(r#"{"cuda":true,"vram_free_mb":2500}"#);
        let ok = json(r#"{"ok":true,"ms":41234,"bytes":90000}"#);
        assert_eq!(
            classify_gate(GateExit::Code(Some(0)), Some(&start), Some(&ok)),
            GateOutcome::Passed { ms: 41234, watermark: None, voice_design: false },
            "透かしを返さない古い sidecar.py は「分からない」"
        );
        // v0.5.7 項目 5: 透かしが効いていたかを読む（合否には使わない）
        for (field, want) in [("true", Some(true)), ("false", Some(false)), ("null", None)] {
            let r = json(&format!(r#"{{"ok":true,"ms":5,"bytes":9,"watermark":{field}}}"#));
            assert_eq!(
                classify_gate(GateExit::Code(Some(0)), Some(&start), Some(&r)),
                GateOutcome::Passed { ms: 5, watermark: want, voice_design: false },
                "{field}"
            );
        }
        // 「ok」と言っても空の合成・0 以外の終了は合格にしない
        let empty = json(r#"{"ok":true,"ms":1,"bytes":0}"#);
        assert!(matches!(
            classify_gate(GateExit::Code(Some(0)), None, Some(&empty)),
            GateOutcome::Failed(_)
        ));
        assert!(!matches!(
            classify_gate(GateExit::Code(Some(1)), None, Some(&ok)),
            GateOutcome::Passed { .. }
        ));
        assert_eq!(
            classify_gate(GateExit::Code(Some(2)), None, Some(&json(r#"{"ok":false,"kind":"oom"}"#))),
            GateOutcome::OutOfMemory
        );
        assert_eq!(
            classify_gate(GateExit::Code(Some(3)), None, Some(&json(r#"{"ok":false,"kind":"no_gpu"}"#))),
            GateOutcome::NoGpu
        );
        assert_eq!(
            classify_gate(
                GateExit::Code(Some(1)),
                None,
                Some(&json(r#"{"ok":false,"kind":"other","error":"FileNotFoundError: model.safetensors"}"#))
            ),
            GateOutcome::Failed("FileNotFoundError: model.safetensors".to_string())
        );
        // **結果の行が無い ＝ 例外にならず落ちた**。開始時の空き VRAM を手がかりに残す
        assert_eq!(
            classify_gate(GateExit::Code(Some(-1073741819)), Some(&start), None),
            GateOutcome::Crashed { code: Some(-1073741819), vram_free_mb: Some(2500) }
        );
        // 結果の行が無くても、終了コードが意味を持つ
        assert_eq!(classify_gate(GateExit::Code(Some(2)), None, None), GateOutcome::OutOfMemory);
        assert_eq!(classify_gate(GateExit::TimedOut, Some(&start), Some(&ok)), GateOutcome::TimedOut);
    }

    /// **GPU は差分で見る**（反証レビュー #1）。前は使えたのに使えなくなったら更新のせいとして戻す。
    /// 元から見えない・分からない環境だけ飛ばす（ユーザー裁定）。
    #[test]
    fn a_gpu_lost_by_the_update_is_rolled_back_but_a_missing_one_is_skipped() {
        assert!(matches!(gate_verdict(&GateOutcome::NoGpu, Some(true)), GateVerdict::RollBack(_)));
        assert!(matches!(gate_verdict(&GateOutcome::NoGpu, Some(false)), GateVerdict::Skip(_)));
        assert!(matches!(gate_verdict(&GateOutcome::NoGpu, None), GateVerdict::Skip(_)));
        assert!(matches!(gate_verdict(&GateOutcome::NoVoiceRef, Some(true)), GateVerdict::Skip(_)));
        assert!(matches!(gate_verdict(&GateOutcome::Passed { ms: 1, watermark: None, voice_design: false }, Some(true)), GateVerdict::Pass(_)));
    }

    /// **VRAM 不足は「移行の失敗」と分け、全部は戻さず保留する**（v0.5.7 項目 6、ユーザー裁定）。例外にならず
    /// 落ちたときは、始めたときの空き VRAM が少なければ VRAM 不足とみなす。
    #[test]
    fn a_vram_shortage_is_told_apart_from_a_broken_update() {
        assert!(matches!(gate_verdict(&GateOutcome::OutOfMemory, None), GateVerdict::HoldForVram(_)));
        assert!(matches!(
            gate_verdict(&GateOutcome::Crashed { code: Some(1), vram_free_mb: Some(900) }, None),
            GateVerdict::HoldForVram(_)
        ));
        assert!(matches!(
            gate_verdict(&GateOutcome::Crashed { code: Some(1), vram_free_mb: Some(12000) }, None),
            GateVerdict::RollBack(_)
        ));
        assert!(matches!(
            gate_verdict(&GateOutcome::Crashed { code: Some(1), vram_free_mb: None }, None),
            GateVerdict::RollBack(_)
        ));
        assert!(matches!(gate_verdict(&GateOutcome::Failed("x".into()), None), GateVerdict::RollBack(_)));
        assert!(matches!(gate_verdict(&GateOutcome::TimedOut, None), GateVerdict::RollBack(_)));
    }

    /// **戻した状態でもう一度試して、更新のせいかを言い分ける**（反証レビュー #2）。
    #[test]
    fn the_recheck_after_the_rollback_says_whose_fault_it_was() {
        let base = || anyhow!("更新したランタイムで合成できませんでした: X");
        let caused = format!("{:#}", explain_after_recheck(base(), &GateOutcome::Passed { ms: 1, watermark: None, voice_design: false }));
        assert!(caused.contains("更新が原因"), "{caused}");
        let before = format!(
            "{:#}",
            explain_after_recheck(base(), &GateOutcome::Failed("FileNotFoundError".into()))
        );
        assert!(before.contains("更新のせいではありません"), "{before}");
        assert!(before.contains("FileNotFoundError"), "{before}");
        let unknown = format!("{:#}", explain_after_recheck(base(), &GateOutcome::OutOfMemory));
        assert!(unknown.contains("分かりません"), "{unknown}");
    }

    /// ゲートの材料: `refs\` の wav から、メイン → サブ → その他、同じ順位なら新しいもの（ユーザー裁定）。
    /// 事前変換の結果（`.latent.pt`）は選ばない。
    #[test]
    fn the_gate_uses_the_newest_main_reference_voice() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(pick_gate_voice_ref(dir.path()), None, "refs が無ければ材料なし");
        let refs = dir.path().join("refs");
        std::fs::create_dir_all(&refs).unwrap();
        let put = |name: &str, age_secs: u64| {
            let path = refs.join(name);
            std::fs::write(&path, b"RIFF").unwrap();
            let when = std::time::SystemTime::now() - Duration::from_secs(age_secs);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
            path
        };
        put("sub_300.wav", 10);
        put("main_100.wav", 1000);
        let newest_main = put("main_200.wav", 100);
        put("main_200.Aratako__x.fp32-fp32.n-16_e1_s30.latent.pt", 1);
        assert_eq!(pick_gate_voice_ref(dir.path()), Some(newest_main));
    }

    /// **生成を試さない呼び出し（戻したあとの確かめ直し）で参照音声が 1 つも無ければ**、合成は確かめず
    /// （python も起動せず）「材料なし」を返す。
    #[test]
    fn without_a_reference_voice_the_recheck_is_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("python").join("python.exe");
        let mut lines: Vec<String> = Vec::new();
        assert_eq!(
            run_synth_gate(dir.path(), &py, &[], false, |l| lines.push(l.to_string())),
            GateOutcome::NoVoiceRef
        );
        assert!(lines.is_empty(), "何も始めていない: {lines:?}");
        assert!(!dir.path().join(GATE_DIR).exists());
    }

    /// **更新のゲートは参照音声が無くても走る**（v0.5.7 項目 6）。生成した声で合成するので、「材料なし」で
    /// 確かめずに通さない（通すと、生成しか手段の無い人が旧モデルを消されたあと戻る先を失う）。
    #[test]
    fn without_a_reference_voice_the_gate_still_runs_with_a_generated_voice() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("python").join("python.exe");
        let mut lines: Vec<String> = Vec::new();
        let got = run_synth_gate(dir.path(), &py, &[], true, |l| lines.push(l.to_string()));
        assert_ne!(got, GateOutcome::NoVoiceRef, "参照音声が無くても確かめに行く");
        assert!(matches!(got, GateOutcome::Failed(_)), "この一時フォルダには python が無いので起動に失敗する: {got:?}");
        assert!(!dir.path().join(GATE_DIR).exists(), "作業場所は片付ける");
    }

    /// ゲートに渡す引数（v0.5.7 項目 6）: 生成も試すなら `--gate-dir`、参照音声の写しがあれば `--voice-ref`。
    #[test]
    fn the_gate_passes_a_work_dir_for_the_voice_design_and_the_copy_if_any() {
        let root = Path::new("R");
        let work = Path::new("R/.update-gate");
        let copy = Path::new("R/.update-gate/ref.wav");
        let joined = |a: Vec<std::ffi::OsString>| {
            a.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
        };
        let both = joined(gate_args(root, work, Some(copy), true, &["--model-synth".into(), "X".into()]));
        assert!(both.contains("--synth-once"), "{both}");
        assert!(both.contains(&format!("--voice-ref {}", copy.display())), "{both}");
        assert!(both.contains(&format!("--gate-dir {}", work.display())), "{both}");
        assert!(both.ends_with("--model-synth X"), "{both}");
        let generated_only = joined(gate_args(root, work, None, true, &[]));
        assert!(generated_only.contains("--gate-dir") && !generated_only.contains("--voice-ref"), "{generated_only}");
        let recheck = joined(gate_args(root, work, Some(copy), false, &[]));
        assert!(recheck.contains("--voice-ref") && !recheck.contains("--gate-dir"), "確かめ直しは合成だけ: {recheck}");
    }

    /// **VRAM 不足で保留したら、記録はパッケージだけ進め、モデルは進めない**（v0.5.7 項目 6）。記録は読み先の正本
    /// なので、モデルを進めると確かめていない新しいモデルを読みに行く。
    #[test]
    fn a_hold_commits_the_packages_but_not_the_models() {
        let outdated: Vec<String> = ["huggingface_hub", "irodori_tts", "model_codec", "model_synth", "model_voice_design", "transformers"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let plan = update_plan(&outdated);
        assert!(plan.models, "前提: モデルも入れ直す計画");
        let held = names_committed_on_hold(&plan);
        assert_eq!(held, vec!["huggingface_hub", "irodori_tts", "transformers"]);
    }

    /// 保留の分岐は**戻さない**（全戻しを呼ばない）で、退避と控えを片付け、パッケージの分だけ記録する（配線を本文の
    /// テキストで固定する。子プロセスの python が要るので単体では通せない）。
    #[test]
    fn the_hold_branch_keeps_the_new_packages_and_records_only_them() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("        GateVerdict::HoldForVram(why) => {").expect("保留の分岐が無い")..];
        let body = &body[..body.find("\n        }\n").unwrap()];
        assert!(!body.contains("roll_back_update"), "保留で全戻しを呼んでいる");
        assert!(body.contains("remove_dir_all(asset_root.join(UPDATE_BACKUP_DIR))"), "退避を片付けていない");
        assert!(body.contains("remove_versions_snapshot(asset_root)"), "版の控えを片付けていない");
        assert!(body.contains("let held = names_committed_on_hold(&plan);"), "記録する名前を絞っていない");
        assert!(body.contains("record_after_install(asset_root, &held,"), "パッケージの分を記録していない");
    }

    /// 生成も確かめたかを読み、合格の文面に出す（v0.5.7 項目 6。項目 10 は、これが真のときだけ旧モデルを消す）。
    #[test]
    fn the_gate_reads_whether_the_voice_design_was_checked() {
        let r = json(r#"{"ok":true,"ms":9000,"bytes":9,"watermark":true,"voice_design":{"ok":true,"ms":1200}}"#);
        let got = classify_gate(GateExit::Code(Some(0)), None, Some(&r));
        assert_eq!(got, GateOutcome::Passed { ms: 9000, watermark: Some(true), voice_design: true });
        let GateVerdict::Pass(msg) = gate_verdict(&got, Some(true)) else { panic!("合格にならない") };
        assert!(msg.contains("参照音声の生成も確かめました"), "{msg}");
        for field in ["null", r#"{"ok":false}"#] {
            let r = json(&format!(r#"{{"ok":true,"ms":1,"bytes":9,"voice_design":{field}}}"#));
            assert!(
                matches!(classify_gate(GateExit::Code(Some(0)), None, Some(&r)), GateOutcome::Passed { voice_design: false, .. }),
                "{field}"
            );
        }
        // 生成で失敗した報告は不合格（更新したモデルで参照音声を作れない）
        let r = json(r#"{"ok":false,"kind":"other","stage":"voice_design","error":"参照音声の生成: X"}"#);
        let got = classify_gate(GateExit::Code(Some(1)), None, Some(&r));
        assert!(matches!(&got, GateOutcome::Failed(why) if why.contains("参照音声の生成")), "{got:?}");
        assert!(matches!(gate_verdict(&got, Some(true)), GateVerdict::RollBack(_)));
    }

    /// **ゲートは取得したいまのビルドの値で試し、戻したあとの確認は読み先で試す**（v0.5.6 項目 3b の配線）。
    /// 取り違えても型は合うのでテストでは捕まらない（実物の python が要る）。旧い読み先で試すと
    /// 「コードだけ新しくて重みが無い」を捕まえられず、戻したあとを新しい値で試すと切り分けにならない。
    #[test]
    fn the_gate_tries_the_new_models_and_the_recheck_tries_the_old_ones() {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body = &src[src.find("pub async fn update_irodori_runtime").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let gate = body
            .find("run_synth_gate(asset_root, &py_exe, &model_args_for_fetch(), true,")
            .expect("ゲートが取得したいまのビルドの値で、生成も含めて試していない（v0.5.7 項目 6）");
        let read = body
            .find("let (read_args, _) = model_args_for_read(asset_root);")
            .expect("戻したあとの確認が読み先を使っていない");
        let recheck = body
            .find("run_synth_gate(asset_root, &py_exe, &read_args, false,")
            .expect("戻したあとにもう一度、合成だけで試していない（v0.5.7 項目 6）");
        assert!(gate < read && read < recheck, "順序が崩れている");
    }

    /// **`sidecar.py` の一発合成は、作業場所を渡されたら先に参照音声を生成し、参照音声が無ければ生成した声で
    /// 合成し、結果に生成の欄を返す**（v0.5.7 項目 6。本文のテキストで固定する）。
    #[test]
    fn the_sidecar_synth_once_tries_the_voice_design_first() {
        let py = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("python").join("sidecar.py"))
            .unwrap().replace("\r\n", "\n");
        let body = &py[py.find("def synth_once(asset_dir: Path, voice_ref: Optional[Path], gate_dir: Optional[Path] = None) -> int:")
            .expect("synth_once が作業場所を受け取らない")..];
        let body = &body[..body.find("\n\n\ndef ").unwrap()];
        let design = body.find("backend.generate_voice_ref(SYNTH_ONCE_CAPTION, generated)").expect("生成を試していない");
        let fallback = body.find("        if voice_ref is None:\n            voice_ref = generated").expect("参照音声が無いとき生成した声を使っていない");
        let synth = body.find("wav = backend.synthesize(SYNTH_ONCE_TEXT, voice_ref, 1.0, None)").expect("合成していない");
        assert!(design < fallback && fallback < synth, "生成 → 生成した声の代用 → 合成の順になっていない");
        assert!(body.contains("\"voice_design\": voice_design,"), "結果に生成の欄を返していない");
        assert!(body.contains("\"stage\": \"voice_design\""), "生成の失敗を見分けられない");
        assert!(py.contains("        return synth_once(asset_dir, args.voice_ref, args.gate_dir)"), "作業場所を渡していない");
        assert!(py.contains("        \"--gate-dir\","), "--gate-dir の引数が無い");
    }

    /// **`sidecar.py` の一発合成と噛み合っていること**（目印の文字列・終了コード・分岐の位置）。
    /// どちらか片方だけ変えると、ゲートは結果を読めず「落ちた」と誤判定する。
    #[test]
    fn the_gate_matches_the_sidecar_synth_once_mode() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("python/sidecar.py"))
            .expect("sidecar.py を読めること").replace("\r\n", "\n");
        assert!(src.contains(&format!("SYNTH_ONCE_START_MARKER = {SYNTH_ONCE_START_MARKER:?}")));
        assert!(src.contains(&format!("SYNTH_ONCE_MARKER = {SYNTH_ONCE_MARKER:?}")));
        assert!(src.contains(&format!("SYNTH_ONCE_OOM = {SYNTH_ONCE_OOM}")));
        assert!(src.contains(&format!("SYNTH_ONCE_NO_GPU = {SYNTH_ONCE_NO_GPU}")));
        assert!(src.contains("SYNTH_ONCE_OK = 0"));
        // HTTP を立てないモードなので、ポート確保と --ready-file の必須チェックより前で分岐すること
        let branch = src.find("    if args.synth_once:").expect("--synth-once の分岐があること");
        let port = src.find("    port = args.port if").expect("ポート確保");
        let ready = src.find("--ready-file が必要です").expect("--ready-file の必須チェック");
        assert!(branch < port && branch < ready, "分岐がポート確保・--ready-file の検査より後にある");
    }

    /// **使われなくなったモデルの置き場所だけを挙げる**（v0.5.7 項目 10）。v3 から v4.1 への更新なら、v3・
    /// v2-VoiceDesign・revision を固定する前のコーデックの写しの 3 つ。前後が同じなら無し。
    #[test]
    fn old_model_dirs_are_those_no_longer_read() {
        let before = v054_baseline_models();
        let now = current_models();
        assert_eq!(
            old_model_dirs(&before, &now),
            vec![
                "Aratako__Irodori-TTS-500M-v2-VoiceDesign".to_string(),
                "Aratako__Irodori-TTS-500M-v3".to_string(),
                "Aratako__Semantic-DACVAE-Japanese-32dim".to_string(),
            ]
        );
        assert!(old_model_dirs(&now, &now).is_empty(), "変わっていなければ何も消さない");
        // 生成と合成が同じ置き場所（v4.1）なら 1 つに数え、いまの読み先に入っているものは挙げない
        let mut half = before.clone();
        half.insert("model_synth".into(), now["model_synth"].clone());
        assert_eq!(
            old_model_dirs(&half, &now),
            vec!["Aratako__Irodori-TTS-500M-v2-VoiceDesign".to_string(), "Aratako__Semantic-DACVAE-Japanese-32dim".to_string()]
        );
    }

    /// **旧モデルの置き場所と、旧モデルのための参照音声の変換結果だけを消す**（v0.5.7 項目 10）。ugg が読んでいた
    /// 証拠の無い置き場所・いまのモデルの変換結果・参照音声そのものは残す。
    #[test]
    fn cleaning_up_removes_only_the_old_models_and_their_latents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let before = v054_baseline_models();
        let now = current_models();
        let model = root.join("model");
        let make_model = |name: &str| {
            std::fs::create_dir_all(model.join(name)).unwrap();
            std::fs::write(model.join(name).join("model.safetensors"), b"x").unwrap();
        };
        for v in before.values().chain(now.values()) {
            make_model(&model_dir_of(v));
        }
        make_model("someone-else");
        let refs = root.join("refs");
        std::fs::create_dir_all(&refs).unwrap();
        let key = |m: &std::collections::BTreeMap<String, String>| {
            format!("{}+{}", model_dir_of(&m["model_synth"]), model_dir_of(&m["model_codec"]))
        };
        let (old_key, new_key) = (key(&before), key(&now));
        let files = [
            "main_1.wav".to_string(),
            format!("main_1.{old_key}.fp32-fp32.n-16_e1_s30.latent.pt"),
            format!("sub_2.{old_key}.fp32-fp32.n-16_e1_s30.latent.pt.tmp"),
            format!("main_1.{new_key}.bf16-fp32.n-16_e1_s30.latent.pt"),
        ];
        for f in &files {
            std::fs::write(refs.join(f), b"x").unwrap();
        }

        let out = clean_up_old_models(root, &before, &now);

        assert_eq!(out.removed_models, old_model_dirs(&before, &now));
        assert_eq!(out.removed_latents, 2);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let mut left: Vec<String> = std::fs::read_dir(&model)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected: Vec<String> = now.values().map(|v| model_dir_of(v)).collect();
        expected.push("someone-else".into());
        expected.sort();
        expected.dedup();
        assert_eq!(left, expected, "いまのモデルと、読んでいた証拠の無いものは残す");
        for f in [&files[0], &files[3]] {
            assert!(refs.join(f).is_file(), "消してはいけない: {f}");
        }
        for f in [&files[1], &files[2]] {
            assert!(!refs.join(f).exists(), "旧モデルの変換結果が残っている: {f}");
        }
    }

    /// **旧モデルは、ゲートで合成と生成の両方を確かめ、記録を書いたあとにだけ消す**（v0.5.7 項目 10）。更新の前に
    /// 読んでいた先を、入れ替える前に控える。保留・全戻しの経路では消さない（旧モデルは全戻しと v3 の材料）。
    /// AppHandle も python も要らないが、ゲートは実物の python が要るので、配線を本文のテキストで固定する。
    #[test]
    fn old_models_are_cleaned_up_only_after_both_checks_and_the_record() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tts/irodori_download.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body = &src[src.find("pub async fn update_irodori_runtime<F>(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let at = |s: &str| body.find(s).unwrap_or_else(|| panic!("無い: {s}"));
        assert!(
            at("let models_before = models_to_read(read_stamp(asset_root).as_ref());") < at("let applied = apply_update_plan("),
            "入れ替える前に、読んでいた先を控えていない"
        );
        let record = at("record_after_install(asset_root, &plan.names,");
        let only_if = at("if matches!(outcome, GateOutcome::Passed { voice_design: true, .. }) {");
        let clean = at("clean_up_old_models(asset_root, &models_before, &now)");
        assert!(record < only_if && only_if < clean, "記録の後・両方を確かめたときだけ、になっていない");
        assert_eq!(body.matches("clean_up_old_models(").count(), 1, "保留や全戻しの経路でも消している");
        // 変換結果の名前の組（合成 + コーデック）は sidecar.py と同じ
        let py = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("python").join("sidecar.py"))
            .unwrap()
            .replace("\r\n", "\n");
        assert!(
            py.contains("model_dir_name(MODEL_REPO_SYNTH, MODEL_REVISION_SYNTH)\n            + \"+\"\n            + model_dir_name(MODEL_REPO_CODEC, MODEL_REVISION_CODEC),"),
            "sidecar.py の変換結果の名前の組が、片付けの探し方（<合成>+<コーデック>）と違う"
        );
    }

    /// **導入と更新の両方のコマンドが、プロセスをまたぐ錠と入口の備えを通る**（v0.5.6 項目 3e・3f の配線）。
    /// 片方だけ直して隣を残す形（初回導入は自分のサイドカーを止めていなかった）を繰り返さないため、
    /// コマンドの本文をテキストで見る（AppState が要るので単体では通せない）。
    #[test]
    fn both_runtime_commands_take_the_cross_process_lock_and_prepare() {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        for name in ["pub async fn update_irodori_runtime", "pub async fn download_irodori_assets"] {
            let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            let body = &body[..body.find("\n}\n").unwrap()];
            let lock = body.find("IrodoriBusyGuard::acquire_for(").unwrap_or_else(|| panic!("{name}: プロセスをまたぐ錠を取っていない"));
            let mkdir = body.find("create_dir_all(").unwrap_or_else(|| panic!("{name}: フォルダを作っていない"));
            assert!(mkdir < lock, "{name}: 錠のファイルを置く前にフォルダを作ること");
            assert!(
                body.contains("prepare_to_replace_runtime("),
                "{name}: 入口の備え（自分のサイドカーを止める・掃除を待つ・生きているものを確かめる）を通っていない"
            );
            assert!(!body.contains("IrodoriBusyGuard::acquire()"), "{name}: プロセスの中だけの錠に戻っている");
        }
        // 入口の備えの順（v0.5.6 項目 3e・4）: 自分のを止める → 起動時の掃除を待つ → 持ち主のいない孤児を
        // 止める → 生きているものが残っていれば始めない。掃除より先に確かめると、止められる孤児で断る。
        let name = "async fn prepare_to_replace_runtime";
        let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("{name}: {needle} が無い"));
        let order = [
            at(".shutdown()"),
            at("wait_for_startup_sweep("),
            at("sweep_orphans("),
            at("live_sidecars("),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{name}: 備えの順が違う {order:?}");
    }

    /// **導入・更新の錠はプロセスをまたぐ**（v0.5.6 項目 3f）。握っている間は、この ugg の中の印も
    /// ファイルの錠も立ったまま。放せば両方外れる。
    #[test]
    fn the_update_lock_holds_both_the_process_flag_and_the_file_lock() {
        let _serial = lock_busy_for_test();
        let dir = tempfile::tempdir().unwrap();
        let guard = IrodoriBusyGuard::acquire_for(dir.path()).expect("取れる");
        assert!(is_busy(), "握っている間はプロセスの中の印が立っている");
        assert!(
            crate::tts::file_lock::FileLock::is_held_elsewhere(&dir.path().join(UPDATE_LOCK_FILE)),
            "ファイルの錠も握っている（もう 1 つの ugg から見える）"
        );
        drop(guard);
        assert!(!is_busy(), "放したら印も外れる");
        assert!(!crate::tts::file_lock::FileLock::is_held_elsewhere(&dir.path().join(UPDATE_LOCK_FILE)));
    }

    /// **初回導入が全部済んだら、前回の更新の残り（退避と版の控え）を片付ける**（v0.5.6 リリース前監査）。
    /// 残すと、次の更新の入口が、入れたばかりのものを古い退避と控えで書き戻す。残りが無ければ何もしない。
    #[test]
    fn a_full_install_discards_the_leftovers_of_a_failed_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(UPDATE_BACKUP_DIR).join("dacvae")).unwrap();
        std::fs::write(versions_snapshot_path(root), r#"{"tqdm":"4.67.3"}"#).unwrap();
        let mut said = Vec::new();

        assert!(discard_update_leftovers(root, |l| said.push(l.to_string())));
        assert!(!root.join(UPDATE_BACKUP_DIR).exists(), "退避を片付ける");
        assert!(read_versions_snapshot(root).is_none(), "版の控えを片付ける");
        assert_eq!(said.len(), 1, "片付けたことを伝える");

        assert!(!discard_update_leftovers(root, |l| said.push(l.to_string())), "残りが無ければ何もしない");
        assert_eq!(said.len(), 1);
    }

    /// 初回導入のコマンドは、記録を書いた**あと**で残りを片付ける（途中で失敗したら、残りは戻す先のまま）。
    #[test]
    fn the_full_install_discards_leftovers_only_after_recording() {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let name = "pub async fn download_irodori_assets";
        let body = &src[src.find(name).unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let recorded = body.find("record_installed(").expect("記録していない");
        let discarded = body.find("discard_update_leftovers(").expect("残りを片付けていない");
        assert!(recorded < discarded, "記録より先に片付けている");
    }

    /// **合成の側が錠を「試してすぐ放す」一瞬に重なっても、更新は断られない**（v0.5.6 リリース前監査）。
    /// 以前は 1 回だけ試していたので、その一瞬に重なると「もう 1 つの ugg が更新しています」と事実と違う
    /// 理由で断った。ここでは別のハンドルが 100ms だけ握って放す。
    #[test]
    fn a_momentary_probe_does_not_make_the_update_refuse() {
        let _serial = lock_busy_for_test();
        let dir = tempfile::tempdir().unwrap();
        let probe = crate::tts::file_lock::FileLock::try_acquire(&dir.path().join(UPDATE_LOCK_FILE))
            .unwrap()
            .unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(probe);
        });
        let guard = IrodoriBusyGuard::acquire_for(dir.path());
        releaser.join().unwrap();
        assert!(guard.is_ok(), "一瞬の握りで断らない: {:?}", guard.err());
    }

    /// **もう 1 つの ugg が握っていたら始めない**。そのときプロセスの中の印は残さない
    /// （残すと、この ugg の合成まで「更新中」として VOICEVOX へ倒れ続ける）。
    #[test]
    fn another_ugg_holding_the_update_lock_is_refused_without_leaving_a_flag() {
        let _serial = lock_busy_for_test();
        let dir = tempfile::tempdir().unwrap();
        // もう 1 つの ugg の代わりに、別のハンドルで錠を握っておく
        let other = crate::tts::file_lock::FileLock::try_acquire(&dir.path().join(UPDATE_LOCK_FILE))
            .unwrap()
            .unwrap();
        let err = IrodoriBusyGuard::acquire_for(dir.path()).err().expect("取れない");
        assert!(format!("{err:#}").contains("もう 1 つの ugg"), "{err:#}");
        assert!(!is_busy(), "断ったらプロセスの中の印を残さない");
        // 合成の側からは「更新中」に見える
        assert!(is_busy_for(dir.path()), "もう 1 つの ugg の更新を、合成の側も見る");
        drop(other);
        assert!(!is_busy_for(dir.path()), "終われば見えなくなる");
    }

    /// 導入と更新を同時に走らせない（監査 ①）。
    ///
    /// 同じ `site-packages` を 2 経路が触ると、退避 → 入れ直し → 復元のどの段も守れない。
    #[test]
    fn install_and_update_do_not_overlap() {
        let _serial = lock_busy_for_test();
        let first = IrodoriBusyGuard::acquire().expect("1 本目は取れる");
        assert!(
            IrodoriBusyGuard::acquire().is_err(),
            "進行中はもう 1 本走らせない"
        );
        drop(first);
        assert!(
            IrodoriBusyGuard::acquire().is_ok(),
            "終わったら次が取れる（途中で return しても取り残さない）"
        );
    }

    #[test]
    fn move_aside_then_restore_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "irodori_tts", "0.1.0", "OLD");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("irodori_tts");

        move_package_aside(&site, "irodori_tts", &backup).unwrap();
        assert!(!site.join("irodori_tts").exists(), "退避後は元の場所に無い");
        assert!(!site.join("irodori_tts-0.1.0.dist-info").exists());

        restore_package(&site, &backup).unwrap();
        assert_eq!(
            std::fs::read_to_string(site.join("irodori_tts").join("__init__.py")).unwrap(),
            "OLD",
            "中身が戻っていない"
        );
        assert!(
            site.join("irodori_tts-0.1.0.dist-info").join("METADATA").is_file(),
            "dist-info が戻っていない"
        );
    }

    /// **失敗した入れ直しが中途半端に残していても復元できる。**
    /// pip が新しい版を途中まで書いた状態から、旧版へ戻せること。
    #[test]
    fn restore_overwrites_a_half_written_install() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "dacvae", "1.0.0", "OLD");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");
        move_package_aside(&site, "dacvae", &backup).unwrap();

        // 入れ直しが途中で落ちて、新しい版の残骸が居座っている状態を作る。
        std::fs::create_dir_all(site.join("dacvae")).unwrap();
        std::fs::write(site.join("dacvae").join("__init__.py"), "HALF").unwrap();

        restore_package(&site, &backup).unwrap();
        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "OLD",
            "残骸を退けて旧版に戻すこと"
        );
    }

    /// 入っていないパッケージの退避は失敗にしない（新規に入る場合）。
    #[test]
    fn moving_a_missing_package_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("silentcipher");
        move_package_aside(&site, "silentcipher", &backup).expect("失敗にしない");
    }

    /// 似た名前のパッケージを巻き込まない（`dacvae` の退避が `dacvae_extra` を持っていかない）。
    #[test]
    fn move_aside_does_not_touch_similarly_named_packages() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "dacvae", "1.0.0", "TARGET");
        put_pkg(&site, "dacvae_extra", "2.0.0", "BYSTANDER");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");

        move_package_aside(&site, "dacvae", &backup).unwrap();
        assert!(
            site.join("dacvae_extra").join("__init__.py").is_file(),
            "無関係なパッケージを巻き込んでいる"
        );
        assert!(site.join("dacvae_extra-2.0.0.dist-info").is_dir());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_pth_uncomments_import_site() {
        let input = "python311.zip\n.\n\n# Uncomment to run site.main() automatically\n#import site\n";
        let out = patch_pth(input);
        assert!(out.contains("\nimport site\n"));
        assert!(!out.contains("#import site"));
    }

    #[test]
    fn patch_pth_handles_space_before_site() {
        let input = "python311.zip\n.\n# import site\n";
        let out = patch_pth(input);
        // 行頭の "# import site" も import site に置換される
        assert!(out.contains("\nimport site\n"));
        assert!(!out.contains("# import site"));
    }

    #[test]
    fn patch_pth_idempotent_when_already_enabled() {
        let input = "python311.zip\n.\nimport site\n";
        let out = patch_pth(input);
        // 既に有効化されている場合は重複追加しない
        let count = out.matches("\nimport site").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn patch_pth_appends_when_missing() {
        let input = "python311.zip\n.\n";
        let out = patch_pth(input);
        assert!(out.contains("\nimport site\n"));
    }

    #[test]
    fn assets_ready_false_when_python_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!assets_ready(tmp.path()));
    }
}

#[cfg(test)]
mod quoting_tests {
    use super::*;

    /// **PowerShell の単引用符は 2 つ重ねて escape する (v0.5.3)。**
    /// 素通しすると `O'` のようなユーザー名のパスで引用が壊れ、導入が失敗する
    /// (Codex レビュー 2026-09-06)。
    #[test]
    fn apostrophe_in_path_is_doubled() {
        let p = Path::new(r"C:\Users\O'Neil\AppData\Local\ugg");
        let q = ps_single_quoted(p);
        assert!(q.contains("O''Neil"), "escape されていない: {q}");
        // 単引用符が偶数個 = PowerShell の文字列が途中で閉じない。
        assert_eq!(q.matches('\'').count() % 2, 0, "引用が閉じない: {q}");
    }

    #[test]
    fn ordinary_path_is_unchanged() {
        let p = Path::new(r"C:\Users\shiho\AppData\Local\ugg");
        assert_eq!(ps_single_quoted(p), r"C:\Users\shiho\AppData\Local\ugg");
    }
}
