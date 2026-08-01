//! kikitori クライアント: マイク → エンジン → 画面下部バーに逐次表示 →
//! 停止時に wtype 入力（Windows の「聞き取りバー」方式）。
//!
//! keyboard_interactivity は None にして、入力先アプリのフォーカスを奪わない。
//!
//! 実行: kikitori [--socket PATH]（kikitorid が起動済みであること）
//! スポーン型: 1 回目の起動が録音セッション、2 回目の起動は停止指示
//! （制御ソケット接続）。停止時は確定テキストを wtype で入力して終了する。

use std::io::{BufReader, BufWriter, Write as _};
use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::widget::{column, container, text};
use iced::{Color, Element, Length, Subscription, Task};
use iced_layershell::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use kikitori_client::audio::{downmix, downsample, f32_to_s16le};
use kikitori_client::engine_endpoint;
use kikitori_proto::{self as proto, read_frame, write_frame};

const TARGET_RATE: u32 = 16_000;
const BAR_HEIGHT: u32 = 76;

fn ctl_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    format!("{dir}/kikitori-ctl.sock")
}

pub fn main() -> Result<(), iced_layershell::Error> {
    // 常駐しないスポーン型: 1 回目の起動が録音セッションそのもの。
    // 2 回目の起動は先行インスタンスに合図（=停止）して即終了する。
    // エンジンが常駐なので、クライアントを残しておく理由がない
    if std::os::unix::net::UnixStream::connect(ctl_path()).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(ctl_path()); // 異常終了の残骸
    application(App::default, namespace, update, view)
        .style(style)
        .subscription(subscription)
        .settings(Settings {
            layer_settings: LayerShellSettings {
                size: Some((0, BAR_HEIGHT)),
                anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
                layer: Layer::Overlay,
                // バーを触れないようにし、入力先のフォーカスを奪わない
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
    last_commit: String,
    partial: String,
    recording: bool,
}

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    Engine(EngineEvent),
}

#[derive(Debug, Clone)]
enum EngineEvent {
    Status(String),
    Partial(String),
    Commit(String),
    Recording(bool),
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

fn update(app: &mut App, message: Message) -> Task<Message> {
    if let Message::Engine(event) = message {
        match event {
            EngineEvent::Status(s) => app.status = s,
            EngineEvent::Partial(t) => app.partial = t,
            EngineEvent::Commit(t) => {
                app.last_commit = t;
                app.partial.clear();
            }
            EngineEvent::Recording(b) => app.recording = b,
        }
    }
    Task::none()
}

fn view(app: &App) -> Element<'_, Message> {
    if !app.recording {
        // 待機中はバーを消す（背景も style 側で透明にする）
        return container(column![]).into();
    }
    let line = |t: &str, dim: bool| {
        text(t.to_owned())
            .size(20)
            .color(if dim {
                Color::from_rgb(0.6, 0.6, 0.6)
            } else {
                Color::WHITE
            })
            .width(Length::Fill)
    };
    let shown = if app.partial.is_empty() && app.last_commit.is_empty() {
        line(&app.status, true)
    } else {
        line(&app.partial, false)
    };
    container(column![line(&app.last_commit, true), shown].spacing(4))
        .padding([8, 16])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn style(app: &App, _theme: &iced::Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: if app.recording {
            Color::from_rgba(0.08, 0.08, 0.10, 0.85)
        } else {
            Color::TRANSPARENT
        },
        text_color: Color::WHITE,
    }
}

fn subscription(_: &App) -> Subscription<Message> {
    Subscription::run(engine_stream)
}

fn engine_stream() -> impl futures::Stream<Item = Message> {
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
    let events = EventSender(sender);
    let status = |s: &str| events.send(EngineEvent::Status(s.into()));
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
        status("入力デバイスがない");
        return;
    };
    let config = pick_input_config(&device);
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    if !rate.is_multiple_of(TARGET_RATE) {
        status(&format!("サンプルレート {rate} 非対応"));
        return;
    }
    let factor = (rate / TARGET_RATE) as usize;
    eprintln!(
        "[overlay] 入力: {} ({channels}ch {rate}Hz)",
        device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_default()
    );
    let stream = device
        .build_input_stream(
            config.config(),
            move |data: &[f32], _| {
                let mono = downmix(data, channels);
                let _ = audio_tx.send(f32_to_s16le(&downsample(&mono, factor)));
            },
            |e| eprintln!("[overlay] キャプチャエラー: {e}"),
            None,
        )
        .expect("入力ストリームを開けない");
    stream.play().expect("キャプチャ開始に失敗");

    let conn = match kikitori_proto::endpoint::Connection::connect(&endpoint) {
        Ok(c) => c,
        Err(e) => {
            status(&format!("エンジン {endpoint:?} に接続できない: {e}"));
            return;
        }
    };
    let mut writer = BufWriter::new(conn.writer);
    let mut reader = BufReader::new(conn.reader);
    write_frame(&mut writer, proto::HELLO, br#"{"version":0}"#).unwrap();
    write_frame(&mut writer, proto::START, b"{}").unwrap();
    writer.flush().unwrap();
    events.send(EngineEvent::Recording(true));
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

    // 表示を録音状態にし、次の起動（= 停止指示）を制御ソケットで待つ
    events.send(EngineEvent::Recording(true));
    status("録音中…");
    let (ctl_tx, ctl_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let listener = std::os::unix::net::UnixListener::bind(ctl_path())
            .expect("制御ソケットに bind できない");
        if let Ok((conn, _)) = listener.accept() {
            drop(conn); // 接続 = 停止指示
            let _ = ctl_tx.send(());
        }
    });

    // 送信ループ: 停止指示が来るまで AUDIO を流し続ける
    loop {
        if ctl_rx.try_recv().is_ok() {
            events.send(EngineEvent::Recording(false));
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
