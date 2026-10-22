# gpsmon-rs

gpsd 付属の `gpsmon` を参考に Rust で書き直した NMEA 0183 モニタ。
ネットワーク越しの gpsd、または raw NMEA を流す TCP ポート（シリアル-TCP ブリッジや
NTP アプライアンスの NMEA 出力など）に接続して、衛星・測位・センテンス統計・生データを表示する。

外部 crate に依存しない（std のみ）。macOS / Linux / *BSD で `cargo build` だけで通る。

## ビルド

```sh
cargo build --release
./target/release/gpsmon-rs ntp.home.tany.org:4001
```

Raspberry Pi などへは、ターゲット上でそのままビルドするか、macOS から
`cargo zigbuild --release --target aarch64-unknown-linux-gnu` 等でクロスビルドする。
Rust 1.80 以上。

## 使い方

```
gpsmon-rs [options] [target]

target (default localhost:2947):
  host[:port]            gpsd JSON / raw NMEA を自動判別
  host:port:/dev/ttyX    gpsd、指定デバイスのみ WATCH
  [ipv6]:port            IPv6 リテラル
  tcp://host:port        raw NMEA 固定（相手には一切書き込まない）
  gpsd://host[:port]     gpsd 固定

options:
  -r, --raw              raw NMEA として扱う（tcp:// と同じ）
  -g, --gpsd             gpsd として扱う
  -l, --log FILE         受信バイト列をそのまま FILE に追記
  -f, --replay FILE      キャプチャ再生（時刻で 1 epoch/s にペーシング、'-' は stdin で非ペーシング）
  -a, --ascii            罫線を ASCII に（曖昧幅を全角扱いする端末向け）
  -j, --json             起動時から gpsd JSON を Raw 窓に表示
      --snapshot SECS    TUI なしで SECS 秒受信して 1 画面をテキスト出力して終了
      --size WxH         --snapshot の画面サイズ（既定 100x40）
```

キー操作: `q` 終了 / `p`（またはスペース）Raw 窓の一時停止 / `c` 統計クリア / `j` JSON 行の表示切替

## 自動判別

接続直後に相手から届く最初の非空白バイトで判定する。

- `{` → gpsd（VERSION バナー）。`?WATCH={"enable":true,"json":true,"raw":1,"pps":true}` を送る
- `$` / `!` → raw NMEA。以後も相手には何も書き込まない
- 何も来ない場合、ポートが 2947 のときだけ 2 秒後に gpsd とみなして WATCH を送る。
  それ以外のポートでは書き込まずにデータを待ち続ける（GNSS 受信機のシリアル入力にゴミを流さないため）

15 秒無受信、または切断時は自動再接続（1→2→4→8 秒バックオフ、データを受信できたらリセット）。

## 画面

- **Satellites**: 系統 / PRN / 仰角 / 方位 / C/N0 / 測位使用。NMEA 4.10+ の GSV signal ID ごとの C/N0 は `/` 区切り。
  GSA の system ID（field 18）優先、無ければ talker、GN talker なら PRN 範囲で系統判定。
  GPS talker 内の SBAS（33-64, 120-158）と QZSS（193-202）は別系統として表示
- **Fix**: RMC/GGA/GNS/GLL/ZDA/VTG/GSA/GST/TXT から集約
  - `Lag (host-fix)`: 各 epoch 最初の時刻付きセンテンスの受信ホスト時刻 − センテンス内 UTC。
    受信機の出力遅延＋ネットワーク遅延＋ホスト時計誤差の合計で、NMEA の送出タイミング確認用
- **Sentences**: アドレス別の件数と出力周期（Intvl）。周期は NMEA epoch（RMC/GGA 等の時刻変化）単位の到着間隔の EWMA で、
  GSV のように 1 epoch に複数届くものも 1 回と数える。周期の 3 倍（最低 3 秒）途絶すると経過秒数を黄色で `12s!` 表示。
  チェックサム不一致があると赤
- **gpsd**（gpsd 接続時のみ）: VERSION、DEVICES、TPV、PPS / TOFF の `real − clock`
  （PPS は µs、TOFF は ms 表示。正なら system clock が遅れている）
- **Raw data**: ホスト受信時刻（UTC, ms）付きの生行。チェックサム無しは黄、不一致は赤

## 本家 gpsmon との違い

- NMEA 0183 のみ。UBX / SiRF / TSIP などのバイナリドライバ別パネルは無い
- シリアルデバイス直結は未実装（ネットワーク専用）。デバイスへのコマンド送信（ボーレート変更等）も無し
- gpsd 接続時は `raw:1` を使うのでバイナリプロトコルは hex 行として Raw 窓に出るだけ

## ライセンス

BSD-2-Clause（gpsd に合わせた）。コードは gpsmon の挙動を参考にした新規実装で、gpsd のソースは含まない。
