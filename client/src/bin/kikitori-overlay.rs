//! オーバーレイクライアント: 画面下部の layer-shell バーに部分/確定テキストを
//! 逐次表示する（Windows の「聞き取りバー」方式）。
//!
//! v0 は表示のみの検証用マイルストーン（確定入力は kikitori-cli --wtype で）。
//! keyboard_interactivity は None にして、入力先アプリのフォーカスを奪わない。
//!
//! 実行: kikitori-overlay [--socket PATH]（kikitorid が起動済みであること）
//! トグル: `kikitori-toggle` が制御ソケット（kikitori-ctl.sock）に
//! "toggle" を書くと録音開始/停止。停止時は確定テキストを wtype で入力する。

use std::io::{BufReader, BufWriter, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::widget::{column, container, text};
use iced::{Color, Element, Length, Subscription, Task};
use iced_layershell::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use kikitori_client::audio::{downmix, downsample, f32_to_s16le};
use kikitori_proto::{self as proto, read_frame, write_frame};

const TARGET_RATE: u32 = 16_000;
const BAR_HEIGHT: u32 = 76;

pub fn main() -> Result<(), iced_layershell::Error> {
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
    "kikitori-overlay".into()
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
        }
    }
    Task::none()
}

fn view(app: &App) -> Element<'_, Message> {
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

fn style(_app: &App, _theme: &iced::Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::from_rgba(0.08, 0.08, 0.10, 0.85),
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
    let socket = {
        let mut args = std::env::args().skip(1);
        let mut path = None;
        while let Some(a) = args.next() {
            if a == "--socket" {
                path = args.next();
            }
        }
        path.unwrap_or_else(|| {
            let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
            format!("{dir}/kikitori.sock")
        })
    };

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
    let stream = device
        .build_input_stream(
            config.config(),
            move |data: &[f32], _| {
                let mono = downmix(data, channels);
                let _ = audio_tx.send(f32_to_s16le(&downsample(&mono, factor)));
            },
            |e| eprintln!("キャプチャエラー: {e}"),
            None,
        )
        .expect("入力ストリームを開けない");
    stream.play().expect("キャプチャ開始に失敗");
    status("待機中（kikitori-toggle で開始）");

    let conn = match UnixStream::connect(&socket) {
        Ok(c) => c,
        Err(e) => {
            status(&format!("エンジンに接続できない: {e}"));
            return;
        }
    };
    let mut writer = BufWriter::new(conn.try_clone().unwrap());
    let mut reader = BufReader::new(conn);
    write_frame(&mut writer, proto::HELLO, br#"{"version":0}"#).unwrap();
    write_frame(&mut writer, proto::START, b"{}").unwrap();
    writer.flush().unwrap();

    // 受信スレッド
    {
        let events = events.clone();
        let mut session: Vec<String> = Vec::new();
        std::thread::spawn(move || loop {
            let Ok(frame) = read_frame(&mut reader) else {
                eprintln!("[overlay] エンジン切断");
                events.send(EngineEvent::Status("エンジン切断".into()));
                return;
            };
            eprintln!("[overlay] 受信 0x{:02X}", frame.kind);
            let get = |p: &[u8]| {
                serde_json::from_slice::<serde_json::Value>(p)
                    .ok()
                    .and_then(|v| v["text"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            };
            match frame.kind {
                proto::READY => events.send(EngineEvent::Status("待機中".into())),
                proto::PARTIAL => events.send(EngineEvent::Partial(get(&frame.payload))),
                proto::COMMIT => {
                    let t = get(&frame.payload);
                    session.push(t.clone());
                    events.send(EngineEvent::Commit(t));
                }
                proto::STOPPED => {
                    // 停止確定: このセッションの全文を wtype で入力
                    let text: String = session.drain(..).collect();
                    eprintln!(
                        "[overlay] STOPPED: {} 文字を wtype へ",
                        text.chars().count()
                    );
                    if !text.is_empty() {
                        let st = std::process::Command::new("wtype").arg(&text).status();
                        eprintln!("[overlay] wtype 結果: {st:?}");
                        let ok = st.map(|s| s.success()).unwrap_or(false);
                        events.send(EngineEvent::Status(if ok {
                            "入力しました（kikitori-toggle で再開）".into()
                        } else {
                            "wtype 失敗".into()
                        }));
                    } else {
                        events.send(EngineEvent::Status("待機中".into()));
                    }
                }
                _ => {}
            }
        });
    }

    // 制御ソケット: "toggle" 1 行で録音開始/停止
    let (ctl_tx, ctl_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
        let path = format!("{dir}/kikitori-ctl.sock");
        let _ = std::fs::remove_file(&path);
        let listener =
            std::os::unix::net::UnixListener::bind(&path).expect("制御ソケットに bind できない");
        for conn in listener.incoming().flatten() {
            drop(conn); // 接続 = トグル（中身は読まない）
            let _ = ctl_tx.send(());
        }
    });

    // 送信ループ: 録音中だけ AUDIO を流す。トグルで START/STOP
    let mut recording = false;
    loop {
        if ctl_rx.try_recv().is_ok() {
            recording = !recording;
            let kind = if recording { proto::START } else { proto::STOP };
            if write_frame(&mut writer, kind, b"{}").is_err() {
                return;
            }
            let _ = writer.flush();
            eprintln!(
                "[overlay] toggle → {}",
                if recording { "録音" } else { "停止" }
            );
            if recording {
                events.send(EngineEvent::Commit(String::new())); // 前回表示のクリア
                events.send(EngineEvent::Partial(String::new()));
                status("録音中…");
                while audio_rx.try_recv().is_ok() {} // 溜まった音声を捨てる
            }
        }
        match audio_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(bytes) if recording => {
                if write_frame(&mut writer, proto::AUDIO, &bytes).is_err() {
                    return;
                }
                let _ = writer.flush();
            }
            Ok(_) => {}
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
