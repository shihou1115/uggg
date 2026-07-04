<!-- ugg 台本サンプル -->
# ugg 台本サンプル

このファイルはテキスト読み上げツール（台本形式）の動作確認用サンプルです。
読み上げパネルを開いた状態で、この `.md` ファイルを ugg のウインドウに
ドラッグ&ドロップすると、メイン（host）とサブ（guest）の 2 体が
台本どおりに掛け合いで読み上げます。

このフェンス外の本文は読み上げには使われません（自由なメモ欄です）。

```json defaults
{ "default_pause_seconds": 0.3 }
```
```json speakers
{ "host": { "slot": "main" }, "guest": { "slot": "sub" } }
```
```json lines
[
  { "speaker": "host",  "text": "ねえ、聞いた？新しい台本の読み上げができるようになったんだって。" },
  { "speaker": "guest", "text": "知ってるわ。二人の声で交互に読めるのよね。", "pause_after": 0.6 },
  { "speaker": "host",  "text": "そうそう。ここはゆっくりめに読んでみるね。", "speed": -0.2 },
  { "speaker": "guest", "text": "わたしは少し速めで。", "speed": 0.2 },
  { "speaker": "host",  "text": "すごいっ！これで劇みたいにできるね😊", "caption": "うれしそうに、明るく" }
]
```
