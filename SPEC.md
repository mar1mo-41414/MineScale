# MineScale-Java 技術仕様書 v1.0

## 概要

MineScale-Java は Minecraft Java Edition 専用の P2P ワールド共有ツールである。
ポート開放・アカウント登録・VPN 設定なしで、URL を送るだけでワールドへ参加できる体験を提供する。

---

## 設計思想

> **Minecraft 版 AirDrop**

- ネットワーク知識ゼロのユーザーが使える UX を最優先とする
- Coordination Server はシグナリングのみを担い、ゲームデータを一切見ない
- 通常通信は P2P。サーバーはホールパンチングの補助に徹する
- VPN ツールに見えてはいけない。Minecraft 共有専用ツールであること

---

## システム構成

```
                    ┌─────────────────────────┐
                    │   Coordination Server    │
                    │  (Linux / VPS)           │
                    │─────────────────────────│
                    │  部屋レジストリ          │
                    │  STUN アドレス交換       │
                    │  公開鍵交換              │
                    │  リレーフォールバック     │
                    └────────────┬────────────┘
                                 │ HTTP シグナリングのみ
                    ─────────────┴──────────────
                    │                          │
         ┌──────────┴──────────┐   ┌──────────┴──────────┐
         │     Host Client      │   │     Join Client      │
         │  (mc-share host)     │   │  (mc-share join)     │
         │─────────────────────│   │─────────────────────│
         │  LAN ワールド検出    │   │  ローカル TCP proxy  │
         │  STUN / ホールパンチ │   │  STUN / ホールパンチ │
         │  QUIC サーバー       │◀═▶│  QUIC クライアント   │
         │  → Minecraft :port  │   │  LAN world アナウンス│
         └─────────────────────┘   └─────────────────────┘
```

### Coordination Server の役割（これのみ）

- 部屋の生成・管理・有効期限
- Host / Join 双方の STUN アドレス交換
- TLS 証明書フィンガープリント交換
- リレートークン発行
- UDP ホールパンチ補助（タイミング調整なし。クライアント主導）

**Coordination Server はゲームデータを一切処理しない。**

---

## 接続フロー詳細

### 1. Host 起動

```
mc-share host [-p PORT]
```

1. LAN ワールド自動検出（`224.0.2.60:4445` マルチキャスト受信）
   - 検出失敗時はデフォルト port 25565 を使用
2. X25519 エフェメラル鍵ペアを生成
3. QUIC 用の自己署名 TLS 証明書を生成（rcgen）
4. UDP ソケットをバインドし、STUN サーバーへ Binding Request
   - 外部 IP:port を取得（同一ソケットをホールパンチと QUIC に再利用）
5. Coordination Server へ部屋を登録
   - 送信: `host_pubkey`, `host_stun`, `cert_fingerprint`
   - 受信: `room_id`, `host_token`, `relay_token`, `share_url`
6. Share URL を表示
7. Join 側の出現を 2 秒間隔でポーリング（最大 15 分）
8. Join 情報取得後、UDP ホールパンチング開始
9. ホールパンチ完了後、QUIC サーバーとしてリッスン
10. QUIC ストリームごとに Minecraft サーバーへ TCP 接続・転送

### 2. Join 起動

```
mc-share join <URL or room_id>
```

1. X25519 エフェメラル鍵ペアを生成
2. UDP ソケットをバインドし STUN で外部アドレスを取得
3. Coordination Server に Join を登録
   - 送信: `join_pubkey`, `join_stun`
   - 受信: `host_pubkey`, `host_stun`, `cert_fingerprint`, `relay_token`
4. UDP ホールパンチング開始（Host と同時並行）
5. ホールパンチ完了後、TCP リスナーを `0.0.0.0:25565` で起動
6. LAN ワールドアナウンスを `224.0.2.60:4445` へマルチキャスト送信開始
   - Minecraft のマルチプレイ一覧に自動表示
7. QUIC クライアントとして Host へ接続（TLS cert pinning）
8. Minecraft クライアントからの TCP 接続ごとに QUIC ストリームを開いて転送

---

## ホールパンチングアルゴリズム

### プローブフォーマット

```
4 bytes: b"MCS\x01"
```

### タイムライン

```
t=0    : Join が部屋登録 → プローブ送信開始 (100ms 間隔)
t=X    : Host がポーリングで Join を検出 → プローブ送信開始
t=X+ε  : Host が Join のプローブを受信 → グレース期間開始 (2.5秒)
t=X+Y  : Join が Host のプローブを受信 → グレース期間開始 (2.5秒)
         ↑ グレース期間: 相手側が確実にプローブを受け取れるまで送信継続
t=X+2.5: Host のグレース期間終了 → QUIC サーバー起動
t=X+Y+2.5: Join のグレース期間終了 → QUIC 接続開始
```

### グレース期間の必要性

Host がプローブを 1 つ受信した直後にソケットを QUIC に渡すと、
Join 側 NAT にまだ Host のプローブが届いていない場合がある。
2.5 秒のグレース期間で確実に双方向のホールを開ける。

### NAT タイプ対応表

| NAT タイプ | ホールパンチ | 備考 |
|-----------|------------|------|
| Full Cone | ✅ | 問題なし |
| Address-Restricted Cone | ✅ | 問題なし |
| Port-Restricted Cone | ✅ | 問題なし |
| Symmetric NAT | ❌ | リレーフォールバック使用 |

---

## P2P トンネル仕様

### 使用プロトコル

- **QUIC** (quinn 0.11 / rustls 0.23 / TLS 1.3)
- UDP ホールパンチで開けたポートをそのまま QUIC に引き継ぐ

### 証明書認証

CA チェーンを使わず、**フィンガープリントピニング**を採用。

1. Host が rcgen で自己署名証明書を生成
2. SHA-256 フィンガープリントを Coordination Server 経由で Join に配布
3. Join の TLS 検証器がフィンガープリントのみを検証
4. Coordination Server への MITM なしに認証が成立

### ALPN

```
minescale-1
```

### ストリーム多重化

- Minecraft TCP 接続 1 本 = QUIC 双方向ストリーム 1 本
- 複数プレイヤーの同時接続を自然にサポート

---

## 暗号化

| フェーズ | 方式 | 備考 |
|---------|------|------|
| P2P (通常時) | QUIC / TLS 1.3 | E2E 暗号化。Coordination Server は復号不可 |
| リレー (フォールバック) | なし | リレーサーバーが平文を転送。将来対応予定 |
| 鍵交換 | X25519 Diffie-Hellman | エフェメラル鍵。セッションごとに破棄 |
| セッション鍵導出 | HKDF-SHA256 | 共有秘密から ChaCha20-Poly1305 鍵を導出（将来のリレー暗号化用） |

---

## Coordination Server API

### Base URL
```
https://<domain>
```

### エンドポイント一覧

#### `POST /api/v1/rooms` — 部屋作成

**Request:**
```json
{
  "host_pubkey":       "base64(X25519 public key, 32 bytes)",
  "host_stun":         "203.0.113.1:54321",
  "cert_fingerprint":  "base64(SHA-256 of DER cert)"
}
```

**Response 200:**
```json
{
  "room_id":     "swri3s",
  "host_token":  "base64url(128-bit random)",
  "relay_token": "base64url(128-bit random)",
  "relay_addr":  "203.0.113.1:9090",
  "share_url":   "https://mcs.example.com/swri3s"
}
```

**Rate limit:** 10 requests / IP / minute

---

#### `GET /api/v1/rooms/:room_id/peer` — Join 情報ポーリング

**Header:** `Authorization: Bearer <host_token>`

**Response 200** (Join が到着済み):
```json
{
  "join_pubkey": "base64(X25519 public key, 32 bytes)",
  "join_stun":   "198.51.100.2:62137"
}
```

**Response 204**: まだ誰も Join していない

---

#### `POST /api/v1/rooms/:room_id/join` — 部屋に参加

**Request:**
```json
{
  "join_pubkey": "base64(X25519 public key, 32 bytes)",
  "join_stun":   "198.51.100.2:62137"
}
```

**Response 200:**
```json
{
  "host_pubkey":       "base64(X25519 public key, 32 bytes)",
  "host_stun":         "203.0.113.1:54321",
  "cert_fingerprint":  "base64(SHA-256 of DER cert)",
  "relay_token":       "base64url(128-bit random)",
  "relay_addr":        "203.0.113.1:9090"
}
```

**Rate limit:** 5 requests / IP / second

---

#### `GET /healthz` — ヘルスチェック

**Response 200:** `ok`

---

## リレーサーバープロトコル

TCP ポート 9090 に接続後、テキストハンドシェイクを行う。

```
クライアント → サーバー:
  "RELAY <room_id> <role:host|join> <relay_token>\n"

サーバー → クライアント:
  "OK\n"  または  "ERROR <reason>\n"
```

OK 受信後は Host / Join が対になりバイトをそのまま転送する。

**制限事項 (v1.0)**:
- Minecraft Java 通信のみ想定
- P2P 失敗時のフォールバックとして使用
- リレー実装は v1.0 時点でプロトタイプ段階

---

## LAN ワールドアナウンス仕様

Minecraft Java Edition の LAN world discovery を完全エミュレート。

### マルチキャストアドレス

```
224.0.2.60:4445  (UDP)
TTL = 1 (ローカルネットワーク内のみ)
```

### パケットフォーマット

```
[MOTD]<世界名>[/MOTD][AD]<ポート番号>[/AD]
```

例:
```
[MOTD]MineScale World[/MOTD][AD]25565[/AD]
```

### 送信間隔

1500ms ごとに送信（Minecraft の期待値: 約 1 秒間隔）

### 動作原理

Minecraft はマルチキャストパケットの**送信元 IP** をサーバーアドレスとして使用する。
Join クライアントはこの IP:port に TCP 接続するため、
TCP リスナーは `0.0.0.0:PORT` でバインドする必要がある。

---

## STUN 実装仕様

RFC 5389 Binding Request/Response のみ実装（最小限）。

### 対応属性

- `XOR-MAPPED-ADDRESS` (0x0020) — 優先
- `MAPPED-ADDRESS` (0x0001) — フォールバック

### デフォルト STUN サーバー

```
stun.l.google.com:19302
```

### リトライ

最大 3 回、各タイムアウト 3 秒

---

## 部屋管理

| パラメータ | 値 |
|-----------|-----|
| Room ID 長さ | 6 文字 (a-z0-9) |
| Room ID エントロピー | 約 31 bit |
| トークン長 | 128 bit (URL-safe base64) |
| 未接続部屋の有効期限 | 15 分（バックグラウンドで 60 秒ごとに清掃） |
| 1 部屋あたりの Join 数 | 1（最初の Join のみ受付） |

---

## レート制限

| エンドポイント | 制限 |
|--------------|------|
| `POST /api/v1/rooms` | 10 requests / IP / minute |
| `POST /api/v1/rooms/:id/join` | 5 requests / IP / second |
| `GET /api/v1/rooms/:id/peer` | 20 requests / IP / minute |

IP アドレスは `X-Forwarded-For` ヘッダを優先（リバースプロキシ配置時）。

---

## セキュリティ要件

### 汎用トンネル禁止

- 任意ポートへの転送は実装しない
- Minecraft TCP 通信のみを想定した設計
- リレーサーバーは Minecraft ハンドシェイク検査を実施（パケット ID 0x00 確認）

### 外部公開制限

- Join 側 TCP proxy は `0.0.0.0:PORT` バインドだが、
  QUIC トンネルが唯一の Host への経路であり、外部への直接経路は存在しない
- Coordination Server の HTTP API は localhost のみ listen し、リバースプロキシ経由で公開する

### 認証・認可

- Host トークン: 部屋の所有権証明（ポーリング認証）
- リレートークン: リレー接続の認可（128-bit ランダム、一部屋一使用）
- 証明書フィンガープリント: TLS 中間者攻撃の防止

---

## 動作環境

### クライアント (mc-share)

| OS | アーキテクチャ | 備考 |
|----|--------------|------|
| macOS 12+ | Apple Silicon (arm64) | ネイティブビルド |
| macOS 12+ | Intel (x86_64) | クロスビルド |
| Linux | x86_64 | 静的バイナリ (musl) |
| Linux | arm64 | 静的バイナリ (musl) |
| Windows 10/11 | x86_64 | 将来対応予定 |

### サーバー (mc-share-server)

| OS | アーキテクチャ |
|----|--------------|
| Linux | arm64 (musl 静的バイナリ) |

---

## ビルド情報

### 使用ライブラリ (主要)

| ライブラリ | バージョン | 用途 |
|-----------|----------|------|
| tokio | 1.x | 非同期ランタイム |
| quinn | 0.11 | QUIC 実装 |
| rustls | 0.23 | TLS 1.3 |
| rcgen | 0.13 | 自己署名証明書生成 |
| x25519-dalek | 2.x | X25519 Diffie-Hellman |
| chacha20poly1305 | 0.10 | 対称暗号 (将来のリレー暗号化用) |
| hkdf | 0.12 | 鍵導出関数 |
| sha2 | 0.10 | SHA-256 (cert fingerprint) |
| axum | 0.7 | HTTP サーバーフレームワーク |
| dashmap | 5.x | 並行ハッシュマップ (部屋レジストリ) |
| governor | 0.6 | レート制限 |
| clap | 4.x | CLI 引数パース |
| reqwest | 0.12 | HTTP クライアント |

### Rust バージョン

Rust 1.76 以上

---

## 非実装・非目標

以下は意図的に実装しない:

- 永続 VPN / 仮想 NIC
- アカウント / フレンド / 管理 UI
- 汎用ポート転送 / ファイル共有
- IPv6 (STUN / ホールパンチ)
- リレー時の E2E 暗号化 (v1.0 では未実装)
- Windows ネイティブビルド (v1.0 では未検証)

---

## バージョン履歴

| バージョン | 日付 | 変更内容 |
|-----------|------|---------|
| v1.0 | 2026-05-29 | 初回リリース。P2P ホールパンチング + QUIC トンネル動作確認 |
