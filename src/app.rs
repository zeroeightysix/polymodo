use std::io::ErrorKind;
use windowing::egui;
use windowing::sctk::reexports::client::backend::WaylandError;
use windowing::sctk::shell::wlr_layer::Anchor;
use windowing::{LayerShellOptions, LayerWindowing};

pub async fn run() -> anyhow::Result<()> {
    let (mut eq, mut lst) = LayerWindowing::create(
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
        let dispatched = eq.dispatch_pending(&mut lst)?;
        if dispatched > 0 {
            continue;
        }

        eq.flush()?;

        if !lst.events.is_empty() || lst.ctx.has_requested_repaint() {
            lst.render()?;
        }

        if let Some(events) = eq.prepare_read() {
            let fd = events.connection_fd().try_clone_to_owned()?;
            let async_fd = tokio::io::unix::AsyncFd::new(fd)?;
            let mut ready_guard = async_fd.readable().await?;
            match events.read() {
                Ok(_) => {
                    ready_guard.clear_ready();
                }
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => Err(e)?,
            }
            drop(ready_guard);
        }
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
