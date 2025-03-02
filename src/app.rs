use crate::xdg::DesktopEntry;
use tokio::select;
use tokio::sync::mpsc;
use windowing::client::Client;
use windowing::egui;
use windowing::egui::{Frame, ScrollArea, TextStyle, Ui};
use windowing::sctk::shell::wlr_layer::Anchor;
use windowing::LayerShellOptions;

pub async fn run() -> anyhow::Result<()> {
    let (send, mut recv) = tokio::sync::mpsc::channel(15);

    let mut window = Client::create(
        LayerShellOptions {
            anchor: Anchor::empty(),
            width: 250,
            height: 400,
            ..Default::default()
        },
        App::create(send),
    )
    .await?;

    let mut repaint = false;
    loop {
        select! {
            result = window.update(repaint) => {
                let () = result?;
                repaint = false;
            }
            Some(message) = recv.recv() => {
                window.app().on_message(message);
                repaint = true;
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    InputChanged,
    NucleoResult,
}

struct App {
    search_input: String,
    nucleo: nucleo::Nucleo<&'static DesktopEntry>,
    // desktop_entries: Vec<&'static DesktopEntry>,
    show_entries: Vec<&'static DesktopEntry>,
    tx: mpsc::Sender<Message>,
}

impl App {
    fn create(tx: mpsc::Sender<Message>) -> Self {
        let desktop_entries: Vec<&'static DesktopEntry> = crate::xdg::find_desktop_entries()
            .into_iter()
            .map(|v| &*Box::leak(Box::new(v)))
            .collect();

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let nucleo = {
            let tx = tx.clone();
            nucleo::Nucleo::new(
                config,
                std::sync::Arc::new(move || {
                    let _ = tx.try_send(Message::NucleoResult);
                }),
                None,
                1,
            )
        };
        let injector = nucleo.injector();
        for de in &desktop_entries {
            injector.push(
                *de,
                |de: &&'static DesktopEntry, col: &mut [nucleo::Utf32String]| {
                    col[0] = de.name().into()
                },
            );
        }

        App {
            tx,
            search_input: String::new(),
            nucleo,
            // desktop_entries,
            show_entries: Vec::new(),
        }
    }

    fn on_message(&mut self, message: Message) {
        match message {
            Message::InputChanged => {
                self.nucleo.pattern.reparse(
                    0,
                    self.search_input.as_str(),
                    nucleo::pattern::CaseMatching::Smart,
                    nucleo::pattern::Normalization::Never,
                    false,
                ); // TODO: append
                let status = self.nucleo.tick(0);
                if !status.running && status.changed {
                    // somehow, the worker finished immediately,
                    // so send a NucleoResult to process its change.
                    // normally, this never happens!
                    let _ = self.tx.try_send(Message::NucleoResult);
                }
            }
            Message::NucleoResult => {
                self.nucleo.tick(10);
                let snapshot = self.nucleo.snapshot();
                self.show_entries = snapshot.matched_items(..).map(|i| *i.data).collect();
            }
        }
    }

    fn app_launcher_ui(&mut self, ui: &mut Ui) {
        if ui.text_edit_singleline(&mut self.search_input).changed() {
            let _ = self.tx.try_send(Message::InputChanged);
        }

        let row_height = ui.text_style_height(&TextStyle::Monospace);

        ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(
                ui,
                row_height,
                self.show_entries.len(),
                |ui, rows| {
                    for row in rows {
                        let entry = self.show_entries[row];
                        ui.monospace(entry.name());
                    }
                }
            );
    }
}

impl windowing::app::App for App {
    fn update(&mut self, ctx: &egui::Context) {
        let mut frame = Frame::window(&ctx.style());
        frame.shadow.offset[1] = frame.shadow.offset[0];

        egui::CentralPanel::default()
            .frame(frame.outer_margin((frame.shadow.blur + frame.shadow.spread + 1) as f32))
            .show(ctx, |ui| {
                self.app_launcher_ui(ui);
            });
    }
}
