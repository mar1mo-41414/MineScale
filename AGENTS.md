# AGENTS.md — MineScale-Java で作業する人・エージェント向け

このリポジトリで開発・修正・リリースを行うときの、前提と作法と落とし穴をまとめています。
コードを書く前に読んでください。Claude Code 向けの追加ルールは [CLAUDE.md](CLAUDE.md) にあります。

## 1. これは何か

Minecraft Java Edition 専用の P2P ワールド共有ツール（Rust）。ポート開放・アカウント登録なしで、
URL を送るだけでワールドを友達と遊べる。

- 通常は **UDP ホールパンチング + QUIC / TLS 1.3**（Direct）。
- 直接つながらない回線（Symmetric NAT、UDP 遮断）では、仲介サーバー経由の **TCP リレー**に切り替える。
- 仲介サーバー（Coordination + Relay）は運営者が立てている。クライアントはその URL に依存する。

### 譲れない前提

- **初心者向け**。設定・アカウント・課金なしで使えること。UI に難しい概念を出さない。
- **P2P 優先**。ただしリレーは暗号化されない（§5-3）。説明では経路を区別する。
- **テレメトリはオプトインで、既定オフ。UI から協力を求めない**（§5-8）。
- 汎用トンネル・VPN にしない。Minecraft の共有専用。

## 2. 構成

```
client/   mc-share (CLI) とライブラリ (mc_share)。host / join / tunnel / relay / lan / stun ほか
gui/      mc-share-gui (eframe / egui)。client ライブラリを呼ぶだけの薄い層
server/   mc-share-server (axum)。部屋の管理・シグナリング・TCP リレー・テレメトリ受け口
scripts/  build-all.sh（全 6 ターゲット）、publish-winget.sh（winget への公開）
nat-scanner/  独立ツール。NAT の挙動を測る（本体とは無関係。Python 標準ライブラリのみ）
docs/     TROUBLESHOOTING.md / COMPATIBILITY.md
SPEC.md   技術仕様（実装と合わせて更新する）
HISTORY.md  開発履歴・調査の経緯・教訓
```

主なモジュール（`client/src/`）: `host.rs` / `join.rs`（全体の流れ）、`tunnel.rs`（QUIC・ホールパンチ・
経路選択）、`relay.rs`（リレーのクライアント）、`lan.rs`（LAN アナウンスと接続元の判定）、
`stun.rs`、`transport.rs`（経路の種類: Quic / Relay）、`telemetry.rs`、`coord.rs`（仲介サーバーの API）。

## 3. ビルド・テスト・リリース

```bash
cargo check                      # 全クレート
cargo test -p mc-share           # ユニットテスト
./scripts/build-all.sh           # 全 6 ターゲット → dist/
```

**変更したら、必ず `./scripts/build-all.sh` で全プラットフォームをビルドして通すこと。**
片方の OS でしか出ないエラーがある（例: `socket2` の `with_retries` は Windows に無い）。

- 必要なツールとターゲットは [BUILD.md](BUILD.md) を見る（mingw-w64 / musl-cross / zig + cargo-zigbuild）。
- Linux の **GUI だけは glibc**（`cargo zigbuild --target *-unknown-linux-gnu.2.17`）。musl 版は X11 / Wayland を
  `dlopen` できず起動しない。CLI とサーバーは musl の静的バイナリ。
- GUI の文字列は**英語のみ**。egui の既定フォントに日本語の字形が無く、文字化け（豆腐）になる。

### リリース手順

1. 3 つの `Cargo.toml`（`client` / `gui` / `server`）の `version` を**同じ値**に上げる。
2. `./scripts/build-all.sh`
3. `dist/` から `release-assets/`（git 管理外のステージング）へ、次の名前でコピーする。
   README が `releases/latest` とこのファイル名に依存しているので、**名前を変えない**。

   | 成果物 | ファイル名 |
   |--------|-----------|
   | GUI | `mc-share-gui-windows-x64.exe` / `mc-share-gui-macos` / `mc-share-gui-linux-x64` / `mc-share-gui-linux-arm64` |
   | CLI | `mc-share-cli-windows-x64.exe` / `mc-share-cli-macos` / `mc-share-cli-linux-x64` / `mc-share-cli-linux-arm64` |
   | サーバー | `mc-share-server-linux-x64` / `mc-share-server-linux-arm64` |

4. コミットして push し、`vX.Y.Z` のタグを打ち、`gh release create` で成果物を添付する（ノートは日本語）。
5. **サーバーのコード（`server/`）を変えたときだけ**、仲介サーバーを差し替える。クライアントだけの変更では不要。
   サーバーの `RELAY_ADDR` は、**クライアントから到達できる公開ホスト名**（例: `host.example.com:9090`）にする。
   LAN 内のプライベート IP を入れると、外部の参加者が Relay に届かない。

### winget への公開

`wingetcreate new / update` は、この単一バイナリ（ポータブル exe）を**解析できない**。試さずに、
`scripts/publish-winget.sh [バージョン] [--dry-run]` を使う（マニフェストの生成・fork・PR まで自動）。

- 初回は Microsoft の CLA への同意が要る（PR のコメントで 1 回だけ）。
- 署名なしの exe は Windows Defender に誤検知されやすい。PR が止まったら、Microsoft の誤検知提出
  （<https://www.microsoft.com/en-us/wdsi/filesubmission>）を出してから、検証の再実行を依頼する。
- `gh pr create` が 502 を返しても、PR が作成済みのことがある。再実行する前に既存の PR を確認する。

## 4. 検証の作法

### Minecraft なしで通信を検証する

実際の Minecraft を起動しなくても、トンネルは検証できる。

- 同じマシンで `mc-share host --port 25565` と `mc-share join <URL> --port 0` を動かす。
  **同一マシン同士だと直接接続は成立せず**（ヘアピン）、QUIC を 8 秒待って Relay に切り替わる。
- Relay 専用の動作を見たいときは、両方に `--stun-server 127.0.0.1:9`（誰も待っていないアドレス）を付ける。
- Minecraft の代わりに、ホスト側の `--port` で TCP のエコーサーバーを立てると、バイト列の往復を確かめられる。
- **Relay サーバーは、参加側の最初のバイト列が Minecraft のハンドシェイクらしいか**を見て、
  そうでなければ拒否する（2 バイト目が `0x00`）。テスト用の送信データは `b"\x0f\x00..."` のように始める。
- 本物の仲介サーバーを使う場合、部屋の作成は 1 IP あたり毎分 10 件まで。連打しない。
- 別端末からの見え方（LAN 公開の限定）は、同じ LAN の別の端末から接続・受信を試して確かめる。

### 手動テスト（2 台・2 人で遊ぶ）

- **ホストとジョインで、必ず別のアカウント（別のプレイヤー名）を使う。** 同じ名前で 2 台から入ろうとすると、
  Minecraft が「**その名前は既に使用されています**」と断る。**MineScale の不具合ではない。**
  ひとりで両方の役を試すときは、ジョイン側に**別の Microsoft アカウント**を使う。
  同じ Microsoft アカウントは 2 台から同時に入れない。
- **「LAN に公開」したワールドは、入る側にログイン認証を必ず求める**（Minecraft 1.21.11 の統合サーバーの
  バイトコードで確認: 起動時に認証を有効にし、LAN 公開で変えない）。**ジョイン側がオフラインアカウント
  だと、「無効なセッションです（ゲームとランチャーを再起動してください）」で弾かれる**（これも MineScale の
  不具合ではない）。認証されるのは入る側なので、**ホスト側がオフラインアカウントなのは問題ない**。
  Paper など専用サーバーも、既定（`online-mode=true`）では同じ。オフラインのジョインを試したいときは、
  `online-mode=false` の専用サーバーをホストにする。
- Minecraft 側のエラー文が出たときは、まず**トンネルがホストの Minecraft まで届いているか**を見てから、
  トンネルのコードを疑う（「名前の重複」も「無効なセッション」も、トンネルが届いた結果として返ってきた拒否だった）。
- 一覧にワールドが出ないときは、`127.0.0.1:<port>` を「サーバーを追加」で直接入力すれば入れる。

### 無音になったとき（エラーも出さずにログが止まる）

GUI のログファイルは、画面に出さない DEBUG（この crate のみ）まで記録している。止まった地点の
前後を見て、次を区別する。

| ログの様子 | 意味 |
|-----------|------|
| `PANIC in thread … at <ファイル:行>` が出ている | panic。その行が原因。 |
| `alive`（10 秒ごと）が続き、セッションのログだけ止まる | プロセスは生きていて、何かが止まっている（ハング）。 |
| `alive` も途切れる | プロセスごと終了した（クラッシュ・強制終了）。 |
| `Session ended: …` が画面に出る | タスクが異常終了した（`internal error` は panic）。 |

### 片方が応答しないとき

ジョイン側のログだけを見て結論を出さない。**仲介サーバーの Relay ログで、`role=host` と `role=join` の
どちらの待機が出ているか**を見る。`join` だけなら、ホストのアプリが止まっているか、Relay に届いていない。
その場合は**ホスト側のログファイル**（GUI なら `mc-share-gui.log`）を見て、`ERROR` や `Hole punching failed`
が無いかを確認する。

### 仲介サーバーの接続ログを見る

クライアントが Relay に届いていないとき（`Connection reset` など）は、まず仲介サーバーのログに
`relay parked room=<ID>` / `relay paired` が出ているかを見る。**1 件も無ければ、サーバーの手前（DNS・
経路・ポート）の問題**で、トンネルのロジックではない。そのときは、クライアントの回線から
`getaddrinfo` の結果（A / AAAA）と、各アドレスへの TCP 接続の可否を別々に確かめる。

### ログファイルを見る

GUI・CLI とも、ログを `mc-share.log` / `mc-share-gui.log` に**即時追記**している（場所は
[SPEC.md](SPEC.md) の「ログファイル」）。不具合の報告では、まずこのファイルの該当部分を出してもらう。
新しいログを足すときは `tracing` の `info!` / `warn!` を使えば、画面とファイルの両方に出る。

### マルチキャスト（LAN 一覧）の検証

Python の素のソケットでは、Minecraft の挙動を再現できないことがある（Java は IPv6 のデュアルスタック
ソケットを使うなど、受信の仕方が違う）。**Minecraft 同梱の Java ランタイム**
（`<minecraft>/runtime/java-runtime-*/…/bin/java`）で `MulticastSocket(4445)` + `joinGroup(224.0.2.60)` の
小さなプログラムを動かし、実際に届くかを確かめる。

### 回線の測定

`nat-scanner/` で、回線の NAT が直接接続に向くかを測れる（外部ポートが接続先ごとに変わるかを見る）。
測った範囲では、Symmetric は企業ネットワークに限られた。Symmetric × Symmetric は未測定。

## 5. 設計上の落とし穴（実際に踏んだもの）

経緯は [HISTORY.md](HISTORY.md)。同じ失敗を繰り返さないための要点だけ書く。

1. **常駐タスクを、セッション処理と同じ `tokio::select!` に入れない。**
   「新規参加者の待ち受け」を既存接続の処理と同じ `select!` に入れたため、部屋の期限切れで待ち受けが
   終了すると、既存の接続まで破棄された。独立させたいものは `tokio::spawn` で切り離す。
2. **部屋の寿命（約 15 分）と、トンネルの寿命は別物。** 期限切れ後は、新規の待機（Relay の予備接続）を
   止める。再接続には共有の指数バックオフを入れる（さもないと毎秒数十回の再接続が続く）。
3. **Relay は MineScale では暗号化しない。** ユーザーに見える文面（README・UI・リリースノート）で
   「暗号化されている」「サーバーには見えない」と書くときは、必ず **P2P 経路に限定**する。
   GUI / CLI は経路（`Transport`）を表示する。SPEC と README の食い違いを作らない。
4. **LAN アナウンス（`224.0.2.60:4445` のマルチキャスト）は、カーネル任せのインターフェイス選択に
   依存しない。** 全 IPv4 インターフェイスに `IP_MULTICAST_IF` を明示して、TTL=0 で送る
   （同一ホストの Minecraft にだけ届く）。macOS では VPN が経路を奪うため、既定インターフェイスや
   loopback だけでは届かなかった。Windows は未実測なので、起動時の自己診断が失敗したら TTL=1 に戻る。
   - アナウンスは、**リスナーが立って経路が確定してから**始める（先に出すと、Minecraft の初回 ping が
     接続拒否になり、そのエントリが復活しない）。
   - ローカルプロキシは `0.0.0.0` で待ち受ける（Minecraft はアナウンスの送信元 IP に接続するため）。
     その代わり、**接続元が自機でなければ拒否**する（`lan::is_this_machine`）。既定でこの制限を弱めない。
     `--allow-lan` / GUI のチェックボックスでだけ解除できる。
5. **STUN の失敗は致命的ではない。** UDP が塞がれていても Relay は使える。外部アドレスが無いことは、
   `host_stun` / `join_stun` の**空文字**で表す。これらを読むコードは、パースできない値を
   「直接経路が無い」として扱い、エラーで落とさない。
6. **bool のフラグに clap の `env` を付けない。** clap は厳密な bool として解釈し、`MC_SHARE_TELEMETRY=1`
   が `invalid value '1'` で起動を止める。環境変数は `telemetry::enabled()` が読む。
7. **`if let Ok(..)` でエラーを握りつぶさない。** ホストの QUIC エンドポイントに既定のクライアント設定が
   無く、NAT を開ける処理が常に失敗していたが、握りつぶされて、しばらく気づかなかった。失敗は最低でも
   ログに出す。
8. **テレメトリの約束を守る。**
   - 既定オフ・オプトイン。**UI で協力を求めない**。
   - 送信元 IP やヘッダーを記録しない。外部アドレス・ホスト名・ユーザー名・OS のマイナー版を送らない。
   - 送るフィールドを変えたら、README 末尾の「接続調査への協力について」を必ず同期する。
9. **ホスト名への接続は、全アドレスを並行に試す。** `TcpStream::connect("host:port")` はアドレスを
   1 つずつ順番に試し、それぞれ OS の接続タイムアウトを待つ。ホスト名に応答しない IPv6（AAAA）が
   あると、全体のタイムアウトが先に切れて、IPv4 を試さずに失敗する。Relay への接続は
   `relay::connect_tcp`（IPv4 優先・時間差で並行）を使う。**テストは、IPv4 のみの回線だけでなく、
   IPv6 付きの回線（携帯のテザリングなど）でも行う。**
10. **QUIC には keep-alive を必ず付ける。** quinn は既定では無通信の接続に何も送らず、idle timeout
    （30 秒）で閉じる。UDP の NAT マッピングも同じくらいで消える。`QUIC_KEEP_ALIVE`（10 秒）を
    両端の `TransportConfig` に設定してある。タイムアウトや keep-alive の設定を触ったら、
    `tunnel::tests` の再現テスト（keep-alive 無し=切れる / 有り=切れない）を通すこと。
    「繋がった直後は動くが、少し放置すると `timed out`」は、まずこれを疑う。
11. **UDP の一時的なエラーで、セッションを終わらせない。** Windows では、送信先の NAT が返した
    ICMP「ポート到達不能」が、そのソケットの**次の `recvfrom`** に `WSAECONNRESET`（`ConnectionReset`）
    として現れる。UDP の `recv_from` ループは `tunnel::is_transient_udp_error` で判定して続行する。
    また、ホールパンチの失敗は致命的にしない（QUIC サーバーと Relay の待機は、それでも使える）。
    **macOS / Linux では再現しにくい**（ICMP エラーが次の `send` で消費される）ので、Windows の挙動は
    実機のログで確かめる。
12. **GUI は、セッションが止まったことを必ず見せる。** バックグラウンドのタスクがエラーで終わったとき、
    「Stop」と共有 URL を出したままにすると、落ちているのに動いて見える。終了は `ended_cell` で受けて
    Idle に戻し、理由を表示する（古いセッションの終了通知が新しいセッションを止めないよう、世代番号を持つ）。
13. **「Connected」は、経路が確定してリスナーが立ってから表示する。** QUIC を試す前に「接続した」と
   出すと、Relay に落ちたときに嘘になる。

## 6. ドキュメントのルール

- `README.md` は**日本語**で、簡単な使い方と現在確定している最新情報だけを書く。`README-EN.md` を
  同じ内容で英語に保つ。経緯・詳細・失敗談は載せない。
  - 詳細は `docs/`（トラブルシュート・動作確認）、仕様は `SPEC.md`、経緯と教訓は `HISTORY.md`。
  - 「接続調査への協力について」の節は、README の**末尾**に置く（UI には出さない方針のため）。
- 公開リポジトリなので、**自分の環境に固有の情報を書かない**: ホスト名、IP アドレス、端末名、
  OS の細かいバージョン、特定の回線・サービス名。必要なら一般化する（「Linux」「家庭の光回線」など）。
  トークン・パスワードなどの機密は、どんな場合も書かない・コミットしない。
- 挙動を変えたら、`SPEC.md` と該当する `docs/` を**同じコミットで**更新する。

## 7. コミット

- 1 行目は英語、`scope: 要約` の形（例: `fix(lan): ...` / `feat(gui): ...` / `docs: ...` / `v1.2.x: ...`）。
- 本文には **なぜ**そうしたか、何を**測って**確かめたかを書く。後から読んで再現できるように。
- `dist/` `target/` `release-assets/` はコミットしない。履歴の書き換え・強制 push はしない。

## 8. 作業を終える前のチェックリスト

- [ ] `cargo check` と `cargo test -p mc-share` が通る
- [ ] `./scripts/build-all.sh` が全ターゲットで通る
- [ ] 挙動を変えたなら、実際に動かして確かめた（§4）。ビルドが通っただけで済ませていない
- [ ] `SPEC.md` / `docs/` / `README` の該当箇所を更新した（README は JA / EN の両方）
- [ ] ユーザーに見える文面が、実装より強い主張をしていない（特に暗号化・安全性）
- [ ] 環境固有の情報や機密を、ドキュメント・コミットに含めていない
- [ ] リリースするなら、バージョン・成果物名・サーバー差し替えの要否を確認した（§3）
