use windowing::egui;
use windowing::sctk::shell::wlr_layer::Anchor;
use windowing::LayerShellOptions;
use windowing::client::Client;

pub async fn run() -> anyhow::Result<()> {
    let mut window = Client::create(
        LayerShellOptions {
            anchor: Anchor::empty(),
            width: 250,
            height: 400,
            ..Default::default()
        },
        App::create(),
    )
    .await?;

    loop {
        window.update().await?;
    }
}

// #[derive(Debug, Clone)]
// enum Message {
//     InputChanged(String),
//     NucleoResult,
// }

struct App {
    // search_input: String,
    // nucleo: Nucleo<&'static DesktopEntry>,
    // desktop_entries: Vec<&'static DesktopEntry>,
    // show_entries: Vec<&'static DesktopEntry>,
}

impl App {
    fn create() -> Self {
        // let desktop_entries: Vec<&'static DesktopEntry> = crate::xdg::find_desktop_entries()
        //     .into_iter()
        //     .map(|v| &*Box::leak(Box::new(v)))
        //     .collect();

        // let mut config = nucleo::Config::DEFAULT;
        // config.prefer_prefix = true;
        // let nucleo = Nucleo::new(
        //     config,
        //     Arc::new(move || {
        //         let _ = send.try_send(());
        //     }),
        //     None,
        //     1,
        // );
        // let injector = nucleo.injector();
        // for de in &desktop_entries {
        //     injector.push(
        //         *de,
        //         |de: &&'static DesktopEntry, col: &mut [Utf32String]| col[0] = de.name().into(),
        //     );
        // }

        App {}
    }
}

impl windowing::app::App for App {
    fn update(&mut self, ctx: &egui::Context) {
        // match message {
        //     Message::InputChanged(s) => {
        //         self.search_input = s;
        //         self.nucleo.pattern.reparse(
        //             0,
        //             self.search_input.as_str(),
        //             CaseMatching::Smart,
        //             Normalization::Never,
        //             false,
        //         ); // TODO: append
        //         let status = self.nucleo.tick(0);
        //         if !status.running && status.changed {
        //             // somehow, the worker finished immediately,
        //             // so send a NucleoResult to process its change.
        //             // normally, this never happens!
        //             return Task::done(cosmic::app::Message::App(Message::NucleoResult));
        //         }
        //     }
        //     Message::NucleoResult => {
        //         self.nucleo.tick(10);
        //         let snapshot = self.nucleo.snapshot();
        //         self.show_entries = snapshot.matched_items(..).map(|i| *i.data).collect();
        //     }
        // }
        //
        // Task::none()

        egui::Window::new("Foo").show(ctx, |ui| {
            ui.label("bar");
        });
    }
}
