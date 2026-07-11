# dev 実機検証の「起動待ち / 再起動待ち / 孤児掃除」を決定的に行うヘルパー。
#
# 背景 (2026-07-10 再発防止):
#   dev ログの grep による起動判定が繰り返し誤動作した。
#   - ログは ANSI エスケープ混じりで単語間に色コードが挟まり、パターンが不一致になる
#   - ログは再起動のたび追記されるため、出現回数の閾値判定が無意味になる
#   - 非同期の watch ループはセッション終了で消滅し、何も駆動しない
#   よって判定は「プロセス実体 (dev の exe パス一致 + ウィンドウハンドル + 起動時刻)」で行い、
#   本スクリプトの **同期実行** (タイムアウト付き・exit code で判定) に一本化する。
#   dev ログは診断専用とし、起動判定には使わないこと。
#
# 使い方 (リポジトリルートから):
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev-ready.ps1
#       → dev の ugg.exe がウィンドウを出すまで待つ (既定 120s)。READY で exit 0
#   powershell ... -File scripts/dev-ready.ps1 -AfterTouch src-tauri/src/main.rs
#       → 指定ファイルの更新時刻より「後に起動した」プロセスを待つ (再起動検知)
#   powershell ... -File scripts/dev-ready.ps1 -CleanOrphans
#       → 孤児の dev ugg.exe と、ポート 5273 を握る本リポジトリの vite(node) を停止
#         ("Port 5273 is already in use" で dev が落ちるときは先にこれを実行)
#
# exit code: 0 = READY / CLEANED, 1 = タイムアウト, 2 = 引数エラー

param(
    [int]$TimeoutSec = 120,
    [string]$AfterTouch = "",
    [switch]$CleanOrphans
)

$repo = Split-Path -Parent $PSScriptRoot
$devExe = Join-Path $repo "src-tauri\target\debug\ugg.exe"

function Get-DevProcess {
    @(Get-Process -Name ugg -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and ($_.Path -ieq $devExe)
    })
}

if ($CleanOrphans) {
    $killed = @()
    foreach ($p in Get-DevProcess) {
        try {
            Stop-Process -Id $p.Id -Force -Confirm:$false -ErrorAction Stop
            $killed += "ugg.exe(dev) pid=$($p.Id)"
        } catch {}
    }
    # ポート 5273 (vite devUrl) を握る node のうち、本リポジトリ配下のものだけ止める
    $conns = @(Get-NetTCPConnection -LocalPort 5273 -State Listen -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty OwningProcess -Unique)
    foreach ($ownPid in $conns) {
        $proc = Get-CimInstance Win32_Process -Filter "ProcessId = $ownPid" -ErrorAction SilentlyContinue
        if ($proc -and $proc.Name -eq "node.exe" -and $proc.CommandLine -like "*$repo*") {
            try {
                Stop-Process -Id $proc.ProcessId -Force -Confirm:$false -ErrorAction Stop
                $killed += "node(vite) pid=$($proc.ProcessId)"
            } catch {}
        }
    }
    if ($killed.Count -gt 0) {
        "CLEANED: " + ($killed -join ", ")
    } else {
        "CLEAN: no orphans"
    }
    exit 0
}

$after = $null
if ($AfterTouch -ne "") {
    if (-not (Test-Path $AfterTouch)) {
        "ERROR: AfterTouch のファイルが見つかりません: $AfterTouch"
        exit 2
    }
    $after = (Get-Item $AfterTouch).LastWriteTime
}

$deadline = (Get-Date).AddSeconds($TimeoutSec)
while ((Get-Date) -lt $deadline) {
    foreach ($p in Get-DevProcess) {
        try {
            if ($after -and ($p.StartTime -le $after)) { continue }  # 旧プロセスはスキップ
            $p.Refresh()
            if ($p.MainWindowHandle -ne 0) {
                "READY pid=$($p.Id) started=$($p.StartTime.ToString('HH:mm:ss'))"
                exit 0
            }
        } catch {}  # 判定中にプロセスが消えた場合は次のポーリングへ
    }
    Start-Sleep -Seconds 2
}
"TIMEOUT: dev の ugg.exe が ${TimeoutSec}s 以内にウィンドウを出しませんでした (ビルドエラーの可能性。dev ログを診断用に確認)"
exit 1
