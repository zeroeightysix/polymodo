use crate::fuzzy_search::{FuzzySearch, Row};
use crate::xdg::DesktopEntry;
use tokio::select;
use tokio::sync::mpsc;
use windowing::client::Client;
use windowing::egui;
use windowing::egui::{Frame, Key, ScrollArea, TextEdit, TextStyle, Ui};
use windowing::sctk::shell::wlr_layer::Anchor;
use windowing::LayerShellOptions;

pub async fn run() -> anyhow::Result<()> {
    let (send, mut recv) = mpsc::channel(15);

    let app = App::create(send.clone());
    let search_notify = app.search.notify();

    tokio::spawn(async move {
        loop {
            search_notify.notified().await;
            let _ = send.send(Message::Search).await;
        }
    });

    let mut window = Client::create(
        LayerShellOptions {
            anchor: Anchor::empty(),
            width: 350,
            height: 400,
            ..Default::default()
        },
        app,
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
    Search,
}

struct App {
    search_input: String,
    focus_search: bool,
    search: FuzzySearch<1, SearchRow>,
    show_entries: Vec<SearchRow>,
    tx: mpsc::Sender<Message>,
}

#[derive(Copy, Clone, Debug)]
struct SearchRow(&'static DesktopEntry);

impl Row<1> for SearchRow {
    type Output = &'static str;

    fn columns(&self) -> [Self::Output; 1] {
        [self.name()]
    }
}

impl SearchRow {
    fn name(&self) -> &'static str {
        self.0.name()
    }
}

impl App {
    fn create(tx: mpsc::Sender<Message>) -> Self {
        let desktop_entries: Vec<_> = crate::xdg::find_desktop_entries()
            .into_iter()
            .map(|v| SearchRow(&*Box::leak(Box::new(v))))
            .collect();

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        search.push_all(desktop_entries);

        App {
            tx,
            search_input: String::new(),
            focus_search: true,
            // desktop_entries,
            search,
            show_entries: Vec::new(),
        }
    }

    fn on_message(&mut self, message: Message) {
        match message {
            Message::Search => {
                self.search.tick();
                self.show_entries = self.search.get_matches().into_iter().copied().collect();
            }
        }
    }

    fn app_launcher_ui(&mut self, ui: &mut Ui) {
        let response = TextEdit::singleline(&mut self.search_input)
            .desired_width(f32::INFINITY)
            .show(ui)
            .response;
        if std::mem::replace(&mut self.focus_search, false) {
            response.request_focus();
        }

        if response.has_focus() {
            if ui.input(|input| input.key_pressed(Key::ArrowDown)) {
                // TODO
            } else if ui.input(|input| input.key_pressed(Key::ArrowUp)) {
                // TODO
            }
        }
        // if the text input has changed,
        if response.changed() {
            // make a new search.
            self.search.search::<0>(self.search_input.as_str());
        }

        let row_height = ui.text_style_height(&TextStyle::Monospace);

        ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            row_height,
            self.show_entries.len(),
            |ui, rows| {
                for row in rows {
                    let entry = self.show_entries[row];
                    ui.monospace(entry.name());
                }
            },
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

        // Kill when escape is pressed
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            std::process::exit(0);
        }
    }
}
