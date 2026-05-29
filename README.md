# MineScale-Java

**Minecraft Java Edition向け、ポート開放不要のP2Pワールド共有ツール。**

URLを送るだけで友達があなたのワールドに参加できます。

```
mc-share host          # ホスト側: URLを発行
mc-share join <URL>    # 参加側: URLを渡すだけ
```

---

## コンセプト

Minecraft版 AirDropを目指す。ネットワーク知識ゼロでも使えること。

- ポート開放不要
- アカウント不要
- P2P優先（Coordination Serverはゲームデータを見ない）
- E2E暗号化（QUIC/TLS 1.3）
- LANワールド自動検出 → Minecraftのマルチプレイ画面に自動表示

---

## 通信アーキテクチャ

```
Coordination Server (VPS)
  │  signaling only (HTTP)
  │  NAT情報・鍵交換のみ
  ├── room registry
  ├── peer exchange
  └── relay fallback (P2P失敗時のみ)

Host Client ════════════════ Join Client
  (QUIC over UDP, E2E暗号化)
  cert-pinned TLS 1.3
  X25519 Diffie-Hellman
```

---

## 接続フロー

```
1. mc-share host
   → STUN でパブリックIP:portを取得
   → Coordination Server に部屋を登録 → URLを発行

2. mc-share join <URL>
   → Coordination Server から相手の情報を取得
   → 双方向UDPプローブでホールパンチ
   → QUICセッション確立 (cert pinning で認証)
   → 127.0.0.1:25565 にTCPリスナーを起動
   → マルチキャスト(224.0.2.60:4445)でLANワールドを偽装アナウンス

3. Minecraftのマルチプレイ一覧にワールドが自動表示される
```

---

## ビルド

### 前提
- Rust 1.76+

```bash
# クライアント
cargo build --release -p mc-share

# サーバー
cargo build --release -p mc-share-server
```

クロスコンパイル例（Linux向け, macOSから）:
```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p mc-share
```

---

## 使い方

### ホスト側

LAN公開中のワールドを自動検出:
```bash
mc-share host
```

ポートを直接指定:
```bash
mc-share host --port 25565
```

出力:
```
  ┌────────────────────────────────────────────────┐
  │  World shared! Send this link to your friend:  │
  │                                                  │
  │  https://mcs.example.com/8fk2lm                 │
  └────────────────────────────────────────────────┘
```

### 参加側

```bash
mc-share join https://mcs.example.com/8fk2lm
# または
mc-share join 8fk2lm
```

接続後はMinecraftのマルチプレイ画面にワールドが自動表示される。

---

## Coordination Server のデプロイ

```bash
# 環境変数
export BASE_URL="https://mcs.example.com"
export RELAY_ADDR="203.0.113.1:9090"   # このサーバーのパブリックIP
export LISTEN_HTTP="0.0.0.0:8080"
export LISTEN_RELAY="0.0.0.0:9090"

./mc-share-server
```

### nginx リバースプロキシ例

```nginx
server {
    listen 443 ssl;
    server_name mcs.example.com;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header X-Forwarded-For $remote_addr;
    }
}
```

---

## セキュリティ設計

| 要素 | 実装 |
|------|------|
| 暗号化 | QUIC / TLS 1.3 (P2P) |
| 鍵交換 | X25519 Diffie-Hellman |
| 証明書検証 | SHA-256フィンガープリントピニング |
| Relay認証 | 128-bit ランダムトークン |
| NAT Traversal | STUN + UDPホールパンチング |
| Relay fallback | TCP, Minecraft handshakeバリデーション付き |
| レート制限 | 部屋作成: 10回/分/IP, Join: 5回/秒/IP |
| 部屋有効期限 | 未接続15分で自動削除 |
| 汎用転送禁止 | MinecraftのTCPのみ, 任意ポート/プロトコル不可 |

### Coordination Serverは何も見ない

- ゲームデータはCoordination Serverを通過しない
- P2P確立後はサーバー非関与
- Relayのみ通信を中継するが、暗号化トークン認証で部屋単位に制限

---

## 対応OS

- Windows 10/11
- Linux (x86_64, ARM64)
- macOS (Intel, Apple Silicon)

---

## 非目標

以下は実装しない:
- 永続VPN / 仮想NIC
- 管理UI / アカウントシステム
- フレンド機能
- 汎用トンネル / ファイル共有
- 任意通信転送

---

## ライセンス

MIT
