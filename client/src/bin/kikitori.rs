//! kikitori クライアント: マイク → エンジン → 画面下部バーに逐次表示 →
//! 停止時に wtype 入力（Windows の「聞き取りバー」方式）。
//!
//! keyboard_interactivity は None にして、入力先アプリのフォーカスを奪わない。
//!
//! 実行: kikitori [--socket PATH]（kikitorid が起動済みであること）
//! スポーン型: 1 回目の起動が録音セッション、2 回目の起動は停止指示
//! （制御ソケット接続）。停止時は確定テキストを wtype で入力して終了する。

use std::io::{BufReader, BufWriter, Write as _};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::widget::{column, container, scrollable, text};
use iced::{Color, Element, Length, Subscription, Task};
use iced_layershell::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use kikitori_client::audio::{downmix, downsample, f32_to_s16le};
use kikitori_client::ctl::{self, Claim};
use kikitori_client::engine_endpoint;
use kikitori_client::overlay;
use kikitori_proto::{self as proto, read_frame, write_frame};

const TARGET_RATE: u32 = 16_000;
/// 表示領域（箱）の幅 = 画面幅 / BOX_WIDTH_DIV。
const BOX_WIDTH_DIV: u32 = 4;
/// 回復不能な失敗を箱に表示してから終了するまでの猶予（issue #6）。
const FATAL_DISPLAY: Duration = Duration::from_secs(4);

/// 計測基準点（main 先頭で初期化）。起動レイテンシの実測用（issue #11）。
static T0: OnceLock<Instant> = OnceLock::new();

/// 起動からの経過時間つきでログを出す。
fn trace(label: &str) {
    let t0 = *T0.get_or_init(Instant::now);
    eprintln!("[overlay] +{:>4}ms {label}", t0.elapsed().as_millis());
}

fn ctl_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    format!("{dir}/kikitori-ctl.sock")
}

/// main で bind 済みの制御ソケット。パイプラインスレッドが受け取って
/// 停止指示（次の起動からの接続）を待つ。
static CTL_LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

/// エンジンに到達する前の早期 return / panic で制御ソケットの残骸を
/// 残さないためのガード。listener を取り出さずに drop されたら片付ける。
struct CtlGuard(Option<UnixListener>);

impl Drop for CtlGuard {
    fn drop(&mut self) {
        if self.0.take().is_some() {
            let _ = std::fs::remove_file(ctl_path());
        }
    }
}

pub fn main() -> Result<(), iced_layershell::Error> {
    T0.get_or_init(Instant::now);
    // 数十文字のテキストを流すだけの箱に GPU は不要（issue #11）。
    // iced_layershell が iced のデフォルト機能（wgpu）を要求するため
    // コンパイル時には外せず、ランタイム選択で tiny-skia を既定にする。
    // 実測: 初回描画 41ms（wgpu）→ 4ms（tiny-skia）。環境変数指定があれば尊重。
    // 制約: tiny-skia の表示経路（softbuffer）は Wayland で Xrgb8888
    // （アルファなし）しか使えず、透明ピクセルは黒く塗られる。そのため
    // サーフェスは表示領域（箱）と同寸にし、透明領域を持たない設計とする。
    // スレッド起動前の set_var なので競合しない
    if std::env::var_os("ICED_BACKEND").is_none() {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }
    // 常駐しないスポーン型: 1 回目の起動が録音セッションそのもの。
    // 2 回目の起動は先行インスタンスに合図（=停止）して即終了する。
    // エンジンが常駐なので、クライアントを残しておく理由がない。
    // bind の成否そのものが役割判定（issue #7）: 判定と占有を単一操作に
    // することで、連打時に二重セッションが立つ窓を消す
    match ctl::claim(&ctl_path()).expect("制御ソケットを確保できない") {
        Claim::Stopped => return Ok(()),
        Claim::Recorder(listener) => *CTL_LISTENER.lock().unwrap() = Some(listener),
    }
    trace("GUI 初期化開始");
    application(App::default, namespace, update, view)
        .style(style)
        .subscription(subscription)
        .settings(Settings {
            layer_settings: LayerShellSettings {
                // 箱の幅（画面幅 / BOX_WIDTH_DIV）を決めるには画面幅が要るが、
                // layer shell の幅 0（お任せ）は左右アンカーが必須。そこで
                // まず全幅 1px の足場で起動し、Opened イベントが運んでくる
                // configure 済みサイズから画面幅を学んで、update() で
                // 箱サイズ + 中央アンカーへ切り替える
                size: Some((0, 1)),
                anchor: Anchor::Left | Anchor::Right,
                layer: Layer::Overlay,
                // 箱を触れないようにし、入力先のフォーカスを奪わない
                keyboard_interactivity: KeyboardInteractivity::None,
                events_transparent: true,
                exclusive_zone: 0,
                margin: (0, 0, 0, 0),
                start_mode: StartMode::Active,
            },
            ..Default::default()
        })
        .run()
}

#[derive(Default)]
struct App {
    status: String,
    /// セッション中の確定済みテキスト。確定のたびに箱を伸ばして
    /// 全行を見えるままにする（表示行数は overlay::MAX_ROWS で頭打ち）
    commits: Vec<String>,
    partial: String,
    /// Opened イベント（足場サーフェスの configure）で学んだ画面幅
    screen_width: Option<u32>,
    /// 最後に発行した箱の高さ。同じ高さの SizeChange を連発しないため
    last_height: u32,
    /// 回復不能な失敗。箱に表示する（issue #6）
    error: Option<String>,
}

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    Engine(EngineEvent),
    /// サーフェスの Opened / Resized が運んでくる幅（画面幅の学習用）
    SurfaceWidth(u32),
}

#[derive(Debug, Clone)]
enum EngineEvent {
    Status(String),
    Partial(String),
    Commit(String),
    Fatal(String),
}

/// futures チャンネルへの同期送信ラッパ。
#[derive(Clone)]
struct EventSender(futures::channel::mpsc::Sender<Message>);

impl EventSender {
    fn send(&self, event: EngineEvent) {
        let _ = self.0.clone().try_send(Message::Engine(event));
    }
}

fn namespace() -> String {
    "kikitori".into()
}

/// いま表示している内容の推定表示行数（折り返し込み。view と対で保つ）。
fn visible_rows(app: &App, usable_px: u32) -> u32 {
    if let Some(err) = &app.error {
        return overlay::est_rows(err, usable_px);
    }
    let current = if app.partial.is_empty() && app.commits.is_empty() {
        &app.status
    } else {
        &app.partial
    };
    let commits: u32 = app
        .commits
        .iter()
        .map(|c| overlay::est_rows(c, usable_px))
        .sum();
    commits + overlay::est_rows(current, usable_px)
}

/// 表示内容から箱の高さを見積もり、変わっていれば SizeChange を発行する。
/// 部分認識の折り返しにも追従する（高さが同じ間は何もしない）。
fn resize_to_fit(app: &mut App) -> Task<Message> {
    let Some(w) = app.screen_width else {
        return Task::none();
    };
    let width = w / BOX_WIDTH_DIV;
    let usable = width.saturating_sub(2 * overlay::PADDING_H).max(1);
    let height = overlay::bar_height(visible_rows(app, usable));
    if app.last_height == height {
        return Task::none();
    }
    app.last_height = height;
    Task::done(Message::SizeChange((width, height)))
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Engine(event) => {
            match event {
                EngineEvent::Status(s) => app.status = s,
                EngineEvent::Partial(t) => app.partial = t,
                EngineEvent::Commit(t) => {
                    app.commits.push(t);
                    app.partial.clear();
                    // 表示は MAX_ROWS で頭打ちなので保持もその分だけでよい
                    // （wtype 用の全文はパイプライン側の session が持つ）
                    let excess = app.commits.len().saturating_sub(overlay::MAX_ROWS as usize);
                    app.commits.drain(..excess);
                }
                EngineEvent::Fatal(msg) => app.error = Some(msg),
            }
            return resize_to_fit(app);
        }
        // 足場（全幅 1px）の configure 済みサイズ = 画面幅。一度学んだら
        // 箱サイズ + 中央アンカー（アンカーなし = 両軸センタリング）へ
        // 切り替え、以降のサイズ通知（自分の箱の幅）は無視する
        Message::SurfaceWidth(w) if app.screen_width.is_none() && w > 1 => {
            trace(&format!("画面幅 {w}px を取得 → 中央の箱へ切り替え"));
            app.screen_width = Some(w);
            let width = w / BOX_WIDTH_DIV;
            let usable = width.saturating_sub(2 * overlay::PADDING_H).max(1);
            let height = overlay::bar_height(visible_rows(app, usable));
            app.last_height = height;
            return Task::done(Message::AnchorSizeChange(Anchor::empty(), (width, height)));
        }
        _ => {}
    }
    Task::none()
}

/// 表示領域の箱。サーフェス自体が箱と同寸（透明領域なし）なので、
/// 背景は style() でサーフェス全体に塗る。中の文字は左寄せ。
/// 中身が箱より高くなったら（確定行の折り返し等）、古い行から上へ隠し、
/// 入力中の最下行は常に見せる。container は子を自身の高さにクランプする
/// ため下端揃えでは実現できず、末尾アンカーの scrollable（バー非表示・
/// サーフェス自体が非対話）を使う。
fn bar(content: Element<'_, Message>) -> Element<'_, Message> {
    container(
        scrollable(content)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::hidden(),
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .anchor_bottom(),
    )
    .padding([overlay::PADDING_V as f32, overlay::PADDING_H as f32])
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view(app: &App) -> Element<'_, Message> {
    static FIRST_VIEW: std::sync::Once = std::sync::Once::new();
    FIRST_VIEW.call_once(|| trace("初回 view 呼び出し"));
    // 回復不能な失敗は他の状態と無関係に表示する（issue #6: 無言で
    // 消えると「ホットキーを押しても何も起きない」ように見える）
    if let Some(err) = &app.error {
        return bar(text(err.to_owned())
            .size(overlay::TEXT_SIZE as f32)
            .color(Color::from_rgb(1.0, 0.55, 0.5))
            .width(Length::Fill)
            .into());
    }
    let line = |t: &str, dim: bool| {
        text(t.to_owned())
            .size(overlay::TEXT_SIZE as f32)
            .color(if dim {
                Color::from_rgb(0.6, 0.6, 0.6)
            } else {
                Color::WHITE
            })
            .width(Length::Fill)
    };
    let shown = if app.partial.is_empty() && app.commits.is_empty() {
        line(&app.status, true)
    } else {
        line(&app.partial, false)
    };
    // 確定行は全て並べる。表示行数の上限（overlay::MAX_ROWS）を超えた分は
    // scrollable の末尾アンカーで古い行から隠れる。レイアウト対象は
    // 箱を埋めるのに足る末尾分だけに絞る（1 確定 ≥ 1 行なので十分）
    let start = app.commits.len().saturating_sub(overlay::MAX_ROWS as usize);
    let mut col = column![].spacing(overlay::SPACING as f32);
    for commit in &app.commits[start..] {
        col = col.push(line(commit, true));
    }
    bar(col.push(shown).into())
}

fn style(_: &App, _theme: &iced::Theme) -> iced::theme::Style {
    // サーフェス = 箱なので全面に背景を塗る（tiny-skia はアルファなしの
    // ため不透明になる。wgpu を明示指定した場合のみ半透明）
    iced::theme::Style {
        background_color: Color::from_rgba(0.08, 0.08, 0.10, 0.85),
        text_color: Color::WHITE,
    }
}

fn subscription(_: &App) -> Subscription<Message> {
    Subscription::batch([
        Subscription::run(engine_stream),
        // Opened は multi_window の登録時に configure 済みサイズ付きで
        // 発火する（Resized は登録後の変化時のみ = 初回は来ない）。
        // 保険で Resized も同じ経路に流す
        iced::window::events().map(|(_, event)| match event {
            iced::window::Event::Opened { size, .. } => {
                trace(&format!("Opened {}x{}", size.width, size.height));
                Message::SurfaceWidth(size.width as u32)
            }
            iced::window::Event::Resized(size) => {
                trace(&format!("Resized {}x{}", size.width, size.height));
                Message::SurfaceWidth(size.width as u32)
            }
            _ => Message::SurfaceWidth(0), // 学習ガードで無視される
        }),
    ])
}

fn engine_stream() -> impl futures::Stream<Item = Message> {
    // 注: iced_layershell は購読をレンダラ初期化と並行に起動するため、
    // この時点でレンダラが出来ている保証はない（表示側は「初回 view」で測る）
    trace("GUI 購読開始");
    iced::stream::channel(
        100,
        |sender: futures::channel::mpsc::Sender<Message>| async move {
            // パイプラインは専用スレッドで動かし、イベントは try_send で
            // 直接流し込む（executor 非依存。バッファ溢れ時は表示を落とすだけ）
            std::thread::spawn(move || run_pipeline(sender));
            futures::future::pending::<()>().await
        },
    )
}

/// マイク → デーモン → イベント（kikitori-cli と同じ経路の表示専用版）。
fn run_pipeline(sender: futures::channel::mpsc::Sender<Message>) {
    trace("パイプライン開始");
    // 制御ソケットは main で bind 済み。エンジンに到達できず早期 return
    // する場合はガードが破棄し、次回起動が録音役になれるようにする
    let mut ctl = CtlGuard(CTL_LISTENER.lock().unwrap().take());
    let events = EventSender(sender);
    let status = |s: &str| events.send(EngineEvent::Status(s.into()));
    // 回復不能な失敗: バーに表示し、猶予の後に非ゼロ終了する（issue #6）。
    // 制御ソケットは先に手放し、表示中の再押下が新しい試行になるようにする
    let fatal = |ctl: CtlGuard, msg: String| {
        eprintln!("[overlay] {msg}");
        drop(ctl);
        events.send(EngineEvent::Fatal(msg));
        std::thread::sleep(FATAL_DISPLAY);
        std::process::exit(1);
    };
    let endpoint = engine_endpoint({
        let mut args = std::env::args().skip(1);
        let mut path = None;
        while let Some(a) = args.next() {
            if a == "--socket" {
                path = args.next();
            }
        }
        path
    });

    // キャプチャを先に開始（HANDOFF §4 の教訓）
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>();
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return fatal(ctl, "入力デバイスがない".into());
    };
    let config = pick_input_config(&device);
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    if !rate.is_multiple_of(TARGET_RATE) {
        return fatal(ctl, format!("サンプルレート {rate} 非対応"));
    }
    let factor = (rate / TARGET_RATE) as usize;
    eprintln!(
        "[overlay] 入力: {} ({channels}ch {rate}Hz)",
        device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_default()
    );
    let stream = match device.build_input_stream(
        config.config(),
        move |data: &[f32], _| {
            let mono = downmix(data, channels);
            let _ = audio_tx.send(f32_to_s16le(&downsample(&mono, factor)));
        },
        |e| eprintln!("[overlay] キャプチャエラー: {e}"),
        None,
    ) {
        Ok(s) => s,
        Err(e) => return fatal(ctl, format!("入力ストリームを開けない: {e}")),
    };
    if let Err(e) = stream.play() {
        return fatal(ctl, format!("キャプチャ開始に失敗: {e}"));
    }
    trace("キャプチャ開始（stream.play 完了）");

    let conn = match kikitori_proto::endpoint::Connection::connect(&endpoint) {
        Ok(c) => c,
        Err(e) => {
            // エラー表示の猶予中はキャプチャ不要（キューに溜まるだけ）
            drop(stream);
            return fatal(ctl, format!("エンジン {endpoint:?} に接続できない: {e}"));
        }
    };
    let mut writer = BufWriter::new(conn.writer);
    let mut reader = BufReader::new(conn.reader);
    let sent = write_frame(&mut writer, proto::HELLO, br#"{"version":0}"#)
        .and_then(|()| write_frame(&mut writer, proto::START, b"{}"))
        .and_then(|()| writer.flush());
    if let Err(e) = sent {
        drop(stream);
        return fatal(ctl, format!("エンジンへ送信できない: {e}"));
    }
    trace("エンジンへ START 送信");
    status("録音中…");

    // 受信スレッド
    {
        let events = events.clone();
        let mut session: Vec<String> = Vec::new();
        std::thread::spawn(move || loop {
            let Ok(frame) = read_frame(&mut reader) else {
                // 切断 = 蹴られた（別クライアントが開始した）かエンジン停止。
                // 残っても意味がないので入力せずに終了する
                eprintln!("[overlay] エンジン切断のため終了");
                let _ = std::fs::remove_file(ctl_path());
                std::process::exit(1);
            };
            let get = |p: &[u8]| {
                serde_json::from_slice::<serde_json::Value>(p)
                    .ok()
                    .and_then(|v| v["text"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            };
            match frame.kind {
                proto::READY => {} // 状態表示は START 時の「録音中…」のみ
                proto::PARTIAL => events.send(EngineEvent::Partial(get(&frame.payload))),
                proto::COMMIT => {
                    let t = get(&frame.payload);
                    session.push(t.clone());
                    events.send(EngineEvent::Commit(t));
                }
                proto::STOPPED => {
                    // 停止確定: このセッションの全文を wtype で入力
                    let text: String = session.drain(..).collect();
                    if !text.is_empty() {
                        let st = std::process::Command::new("wtype").arg(&text).status();
                        let ok = st.map(|s| s.success()).unwrap_or(false);
                        if !ok {
                            eprintln!("[overlay] wtype 失敗");
                        }
                    }
                    std::process::exit(0);
                }
                _ => {}
            }
        });
    }

    // 次の起動（= 停止指示）を制御ソケットで待つ
    let (ctl_tx, ctl_rx) = mpsc::channel::<()>();
    // main の bind から accept までの間に届いた停止指示もバックログに
    // 積まれているので、ここで拾える
    let listener = ctl
        .0
        .take()
        .expect("制御ソケットは main で bind 済みのはず");
    std::thread::spawn(move || {
        if let Ok((conn, _)) = listener.accept() {
            drop(conn); // 接続 = 停止指示
            let _ = ctl_tx.send(());
        }
    });

    // 送信ループ: 停止指示が来るまで AUDIO を流し続ける
    loop {
        if ctl_rx.try_recv().is_ok() {
            let _ = std::fs::remove_file(ctl_path());
            if write_frame(&mut writer, proto::STOP, b"{}").is_err() {
                return;
            }
            let _ = writer.flush();
            // 以降は送らない。STOPPED 受信側が wtype して exit する
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        match audio_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(bytes) => {
                if write_frame(&mut writer, proto::AUDIO, &bytes).is_err() {
                    return;
                }
                let _ = writer.flush();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn pick_input_config(device: &cpal::Device) -> cpal::SupportedStreamConfig {
    let supported: Vec<_> = device
        .supported_input_configs()
        .expect("入力構成を取得できない")
        .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
        .collect();
    for &rate in &[TARGET_RATE, 32_000, 48_000, 96_000] {
        for c in &supported {
            if c.min_sample_rate() <= rate && rate <= c.max_sample_rate() {
                return c.with_sample_rate(rate);
            }
        }
    }
    device.default_input_config().expect("既定の入力構成がない")
}
