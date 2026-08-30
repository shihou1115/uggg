//! nar2ugg — 「伺か」シェル (.nar) を ugg シェル形式へ変換する CLI。
//!
//! ## なぜ ugg 本体と分かれているか
//! ugg 本体は 伺か形式を一切パースしない。SERIKO / SHIORI の語彙が本体へ
//! 染み出すのを防ぐため変換はこの独立バイナリで完結させ、出力を通常の
//! シェル DnD で読み込ませる。**ugg 側は無改修**。
//!
//! ## 流れ
//! ```text
//! source::SourceTree  .nar / zip / ディレクトリ → メモリ上のエントリ集合
//!  → shell_def::parse descript.txt + surfaces.txt → ShellDef (伺か語彙はここまで)
//!  → plan::build      ShellDef + CLI 指定 → Plan (画像を読まない)
//!  → imaging::…       アルファ解決 + element 合成 + 共通キャンバスへ配置
//!  → emit::build      ShellManifest + PNG → OutputBundle (メモリ) → ディスク
//! ```
//! `plan` までが画像に触らないので、`list` は画像が壊れたシェルでも一覧を出せる。
//!
//! ## 変換物のライセンス
//! nar2ugg 自身は MIT だが、**変換結果の権利は元シェルの作者にある**。
//! 手元利用に留め、再配布は原作者の許諾を得ること。機械判定はできないので
//! 変換のたびに stderr へ表示する (出力ディレクトリには置けない。emit.rs 参照)。

mod emit;
mod imaging;
mod plan;
mod report;
mod shell_def;
mod source;
mod text;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use imaging::{Rgba, SlotContent};
use plan::PoseSpec;
use report::{Report, Slot, Warning};
use source::SourceTree;

#[derive(Parser)]
#[command(name = "nar2ugg", version, about = "「伺か」シェルを ugg シェルへ変換する")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// シェルの中身を一覧する (変換はしない)。`--pose` を決めるための材料。
    List {
        /// .nar / .zip / 展開済みディレクトリ。
        input: PathBuf,
    },
    /// ugg シェルへ変換する。
    Convert {
        /// .nar / .zip / 展開済みディレクトリ。
        input: PathBuf,
        /// 出力先ディレクトリ。この下に `<id>/shell.json` を作る。
        #[arg(short, long)]
        out: PathBuf,
        /// シェル ID を明示する。省略時は元の名前から自動生成する。
        #[arg(long)]
        id: Option<String>,
        /// pose の割り当てを明示する。例: `--pose main.happy=5 --pose sub.normal=11`
        ///
        /// 伺かには表情番号の標準が無いため、自動割り当ては推測を含む。
        /// `list` で中身を見てからここで指定する。
        #[arg(long = "pose", value_name = "SLOT.NAME=ID")]
        poses: Vec<PoseSpec>,
        /// 相方 (sub) を出力しない。ダミー判定が外れたときの逃げ道。
        #[arg(long)]
        no_sub: bool,
        /// 出力先が空でなくても上書きする。
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List { input } => run_list(&input),
        Command::Convert { input, out, id, poses, no_sub, force } => {
            let opts = plan::PlanOptions { id, poses, no_sub };
            run_convert(&input, &out, &opts, force)
        }
    }
}

/// シェルの中身を一覧する。**画像を一度も読まない**ので、絵が壊れたシェルでも
/// 中身は見られる。「変換に失敗したので何も分からない」を避けるための経路。
fn run_list(input: &Path) -> Result<()> {
    let mut report = Report::default();
    let mut tree = SourceTree::from_path(input)?;
    tree.select_shell_root()?;
    let def = shell_def::parse(&tree, &mut report)?;

    println!("名前: {}", def.name.as_deref().unwrap_or("(不明)"));
    if let Some(author) = &def.author {
        println!("作者: {author}");
    }
    println!(
        "透過: {}",
        if def.use_self_alpha { "PNG のアルファチャンネル" } else { ".pna / 左上 1px の色" }
    );

    println!("
surface ({} 件):", def.surfaces.len());
    for (id, surface) in &def.surfaces {
        let file = surface.file.as_deref().unwrap_or("(element 合成)");
        let collisions = match surface.collisions.len() {
            0 => String::new(),
            n => format!("  collision {n} 件"),
        };
        println!("  {id:>4}  {file}{collisions}");
    }

    if def.aliases.is_empty() {
        println!("
alias: なし (pose 名は推測になります)");
    } else {
        println!("
alias:");
        for a in &def.aliases {
            println!("  {}.{} → {:?}", a.slot, a.name, a.ids);
        }
    }

    // 仮の割り当ても見せる。--pose で何を指定すべきかの材料になる。
    let plan = plan::build(&def, &plan::PlanOptions::default(), &mut report)?;
    println!("
自動割り当て (id: {}):", plan.id);
    print_report(&report);
    Ok(())
}

/// 変換する。
fn run_convert(input: &Path, out: &Path, opts: &plan::PlanOptions, force: bool) -> Result<()> {
    let mut report = Report::default();
    let mut tree = SourceTree::from_path(input)?;
    tree.select_shell_root()?;
    let def = shell_def::parse(&tree, &mut report)?;
    let plan = plan::build(&def, opts, &mut report)?;

    let slots: Vec<(Slot, &plan::SlotPlan)> = std::iter::once((Slot::Main, &plan.main))
        .chain(plan.sub.as_ref().map(|s| (Slot::Sub, s)))
        .collect();

    let mut images: BTreeMap<(Slot, String), Rgba> = BTreeMap::new();
    for (slot, slot_plan) in slots {
        let mut resolved = BTreeMap::new();
        for (pose, surface_id) in &slot_plan.poses {
            let img = imaging::resolve_surface(&tree, &def, *surface_id, &mut report)
                .with_context(|| format!("{slot}.{pose} (surface{surface_id}) の合成に失敗"))?;
            resolved.insert(pose.clone(), img);
        }

        // **測定と処置の分離。** 絵として成立しているかは imaging::inspect が測り、
        // 「main なら失敗 / sub なら省略」という処置はこの 1 箇所だけが決める。
        // 相方はダミー画像 (単色べた塗り) を置く慣習があり、そのまま出すと ugg 上に
        // 見えない当たり判定の板が立つので、省略するのが正しい。
        let default_img = resolved
            .get(&slot_plan.default_pose)
            .with_context(|| format!("{slot} の default_pose の画像が解決できていない"))?;
        if imaging::inspect(default_img) == SlotContent::Empty {
            match slot {
                Slot::Main => anyhow::bail!(
                    "本体の絵が空です。アニメーション前提のシェルで、素の surface に                      絵が入っていない可能性があります (--pose main.normal=<ID> で指定してください)"
                ),
                Slot::Sub => {
                    report.warn(Warning::general(
                        "相方の絵が実体を持たないため characters.sub を省略しました",
                    ));
                    continue;
                }
            }
        }

        // スロットごとに独立した共通キャンバスへ配置する (main と sub をまとめない)。
        for (pose, img) in imaging::lay_out(resolved) {
            images.insert((slot, pose), img);
        }
    }

    // build は返す前に check_bundle で検証する (ディスクではなく「これから書く内容」
    // に対して検証する。書いた後に検証すると、失敗時に既に書いたファイルの後始末と
    // いう宿題が生まれる)。
    let bundle = emit::build(&plan, images, &mut report)?;
    emit::write(&bundle, out, force)?;

    print_report(&report);
    eprintln!(
        "
変換結果の権利は元シェルの作者にあります。手元利用に留め、
         再配布は原作者の許諾を得てください。"
    );
    Ok(())
}

/// 決定表と警告を stderr へ出す。**変換のたびに必ず出す。**
///
/// 推測で当てた pose には印を付ける。伺かには表情番号の標準が無く、
/// 自動割り当ては当たっている保証が無いため、黙って当てたことにしない。
fn print_report(report: &Report) {
    for d in &report.decisions {
        let mark = if d.basis.is_guess() { " (推測)" } else { "" };
        eprintln!("  {}.{} ← surface{}{}", d.slot, d.pose, d.surface_id, mark);
    }
    for w in &report.warnings {
        eprintln!("  警告: {w}");
    }
    let guessed = report.guessed().count();
    if guessed > 0 {
        eprintln!(
            "\n{guessed} 件の pose は推測で割り当てました。\
             意図と違う場合は `--pose main.happy=<ID>` で指定してください。"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn convert_parses_repeated_pose_flags() {
        let cli = Cli::try_parse_from([
            "nar2ugg", "convert", "shell.nar",
            "-o", "out",
            "--pose", "main.happy=5",
            "--pose", "sub.normal=11",
        ])
        .unwrap();
        let Command::Convert { poses, no_sub, force, .. } = cli.command else {
            panic!("convert として解釈されなかった");
        };
        assert_eq!(poses.len(), 2);
        assert_eq!(poses[0].pose, "happy");
        assert_eq!(poses[1].surface_id, 11);
        assert!(!no_sub);
        assert!(!force);
    }

    #[test]
    fn bad_pose_flag_is_rejected_by_clap() {
        let err = Cli::try_parse_from([
            "nar2ugg", "convert", "shell.nar", "-o", "out", "--pose", "happy=5",
        ]);
        assert!(err.is_err());
    }
}
