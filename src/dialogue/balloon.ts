import type { BalloonSlot, SlotName } from "../types";

interface BalloonView {
  root: HTMLElement;
  textEl: HTMLElement;
  /// バルーン内メニュー (spec §4.3.5) のコンテナ。balloon-main のみ持つ。
  menuEl: HTMLElement | null;
  /// この吹き出し枠が現在基準にしているキャラ。main/sub は常に自分自身、
  /// extra はパターンにより main/sub のどちらかに切り替わる (showBalloon で更新)。
  charSlot: SlotName;
}

const views = new Map<BalloonSlot, BalloonView>();

/// index.html に静的配置された `#balloon-main` / `#balloon-sub` / `#balloon-extra` を取得する。
/// 動的 createElement は WebView2 透過レイヤーで描画されないバグがあるため使わない。
function ensureView(slot: BalloonSlot): BalloonView {
  const cached = views.get(slot);
  if (cached) return cached;
  const root = document.getElementById(`balloon-${slot}`);
  if (!root) {
    throw new Error(`balloon-${slot} DOM が見つかりません（index.html 参照）`);
  }
  const textEl = root.querySelector<HTMLElement>(".balloon-text");
  if (!textEl) {
    throw new Error(`balloon-${slot} に .balloon-text 要素がありません`);
  }
  const menuEl = root.querySelector<HTMLElement>(".balloon-menu");
  // extra の初期 charSlot は未使用時は無意味なので main を仮置き (showBalloon で必ず更新される)。
  const charSlot: SlotName = slot === "sub" ? "sub" : "main";
  const view = { root, textEl, menuEl, charSlot };
  views.set(slot, view);
  return view;
}

/// 起動時に呼び出して全 slot の View を取得しキャッシュする。
/// 取得失敗時に boot エラーとして表に出すための事前確認。
export function preallocateBalloons(): void {
  ensureView("main");
  ensureView("sub");
  ensureView("extra");
}

/// 吹き出しを表示状態にして、テキスト書き込み用の `.balloon-text` 要素を返す。
/// `charSlot` = この吹き出しが基準にするキャラ (main/sub は常に自分自身、
/// extra はパターン3で main・パターン4で sub)。位置決めは reposition() を別途呼ぶ。
/// バルーン内メニューの残骸は新しい発話のたびに掃除する (メニューは発話で置き換わる仕様)。
export function showBalloon(slot: BalloonSlot, charSlot: SlotName): HTMLElement {
  const view = ensureView(slot);
  view.textEl.textContent = "";
  if (view.menuEl) view.menuEl.innerHTML = "";
  view.charSlot = charSlot;
  view.root.classList.add("visible");
  reposition(slot);
  return view.textEl;
}

/// バルーン内メニュー (spec §4.3.5) のコンテナを返す。無い slot は null。
export function balloonMenuContainer(slot: BalloonSlot): HTMLElement | null {
  return ensureView(slot).menuEl;
}

export function hideBalloon(slot: BalloonSlot): void {
  const view = views.get(slot);
  if (!view) return;
  view.root.classList.remove("visible");
}

export function hideAllBalloons(): void {
  for (const slot of views.keys()) {
    hideBalloon(slot);
  }
}

/// キャラ左端と吹き出し右端の間隔 (しっぽ 8px を含む)。入力欄の配置計算も共有する。
export const GAP_X = 24;
/// 吹き出し上端をキャラ上端からどれだけ下げるか (キャラ高さ比)。顔の横に来る。
export const HEAD_RATIO = 0.12;
/// ウインドウ端との最小余白。
export const MARGIN = 8;

function rectsOverlap(left: number, top: number, w: number, h: number, o: DOMRect): boolean {
  return left < o.right && left + w > o.left && top < o.bottom && top + h > o.top;
}

/// 吹き出しを基準キャラ (`view.charSlot`、showBalloon で設定済み) の左横に配置する (伺か風)。
/// - 横: キャラ左端から GAP_X 空けて右端を合わせる。キャラが画面左端付近で
///   収まらない場合はキャラの右横へ反転 (.flip、しっぽも反転。spec §4.1.3)
/// - 縦: キャラ上端 + キャラ高さ × HEAD_RATIO (顔の高さ)
/// - main/sub: 相方の吹き出しと重なる場合は main を上へ・sub を下へ退避
/// - extra (掛け合いパターン3/4 の3ターン目、architecture §10.4 案A): 話者キャラ
///   (パターン3=main、パターン4=sub) の横に出した上で、main・sub 両方の吹き出しと
///   重ならないようさらに外側 (上方向) へ退避する
export function reposition(slot: BalloonSlot): void {
  const view = views.get(slot);
  if (!view) return;
  const char = document.getElementById(`char-${view.charSlot}`);
  if (!char) return;
  const rect = char.getBoundingClientRect();
  const w = view.root.offsetWidth || 200;
  const h = view.root.offsetHeight || 60;
  const winW = window.innerWidth;
  const winH = window.innerHeight;

  let left = Math.round(rect.left - GAP_X - w);
  let flip = false;
  if (left < MARGIN) {
    const rightSide = Math.round(rect.right + GAP_X);
    if (rightSide + w <= winW - MARGIN) {
      left = rightSide;
      flip = true;
    } else {
      left = MARGIN; // 両側とも収まらない極端ケースは左置きで clamp
    }
  }
  view.root.classList.toggle("flip", flip);

  let top = Math.round(rect.top + rect.height * HEAD_RATIO);

  if (slot === "main" || slot === "sub") {
    const other = views.get(slot === "main" ? "sub" : "main");
    if (other && other.root.classList.contains("visible")) {
      const o = other.root.getBoundingClientRect();
      if (rectsOverlap(left, top, w, h, o)) {
        top = slot === "main" ? Math.round(o.top - h - MARGIN) : Math.round(o.bottom + MARGIN);
      }
    }
  } else {
    // extra: main・sub 両方と重ならなくなるまで外側 (上方向) へ追い出す。
    // 双方への押し出しが互いに干渉し得るため数回反復する (上限付きで打ち切り、収まらなければ clamp)。
    for (let i = 0; i < 4; i++) {
      let adjusted = false;
      for (const otherSlot of ["main", "sub"] as const) {
        const other = views.get(otherSlot);
        if (!other || !other.root.classList.contains("visible")) continue;
        const o = other.root.getBoundingClientRect();
        if (rectsOverlap(left, top, w, h, o)) {
          top = Math.round(o.top - h - MARGIN);
          adjusted = true;
        }
      }
      if (!adjusted) break;
    }
  }

  if (top + h > winH - MARGIN) top = winH - MARGIN - h;
  if (top < MARGIN) top = MARGIN;
  view.root.style.left = `${left}px`;
  view.root.style.top = `${top}px`;
}

/// 表示中の吹き出し枠を **main → sub → extra の固定順**で再配置する。
/// キャラのドラッグ移動 (charpos.ts) から呼ぶ。
///
/// 順序が固定なのは、退避の依存を一方向にして循環させないため (案A):
/// main/sub は互いだけを避け、**extra は常に自分が退避する側**として main/sub の
/// 確定位置を見て最後に置く。基準キャラが動いた枠だけを再計算すると、相方をドラッグ
/// したときに extra が取り残されて重なる (リリース前レビュー指摘) ため、1 つでも
/// 動いたら可視の全枠を通す。
export function repositionAll(): void {
  for (const slot of ["main", "sub", "extra"] as const) {
    const view = views.get(slot);
    if (view && view.root.classList.contains("visible")) {
      reposition(slot);
    }
  }
}
