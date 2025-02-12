use crate::xdg::DesktopEntry;
use cosmic::app::{Core, Settings};
use cosmic::iced::futures::{SinkExt, Stream};
use cosmic::iced::window::Id;
use cosmic::iced::Length::Fixed;
use cosmic::iced::{stream, Limits};
use cosmic::iced_runtime::core::window::Id as SurfaceId;
use cosmic::iced_runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced_widget::{button, column};
use cosmic::iced_winit::commands::layer_surface::get_layer_surface;
use cosmic::task::batch;
use cosmic::widget::autosize::autosize;
use cosmic::widget::container;
use cosmic::{Application, Element, Task};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Nucleo, Utf32String};
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc::channel;

static WINDOW_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);
static AUTOSIZE_ID: LazyLock<cosmic::iced::id::Id> =
    LazyLock::new(|| cosmic::iced::id::Id::new("autosize"));

pub fn run() -> cosmic::iced::Result {
    cosmic::app::run::<AppModel>(
        Settings::default()
            .antialiasing(true)
            .client_decorations(true)
            .debug(true)
            .default_text_size(16.0)
            .no_main_window(true)
            .exit_on_close(false),
        (),
    )?;

    Ok(())
}

#[derive(Debug, Clone)]
enum Message {
    InputChanged(String),
    NucleoResult,
}

struct AppModel {
    core: Core,
    search_input: String,
    nucleo: Nucleo<&'static DesktopEntry>,
    desktop_entries: Vec<&'static DesktopEntry>,
    show_entries: Vec<&'static DesktopEntry>,
}

impl Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = env!("CARGO_PKG_NAME");

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<cosmic::app::Message<Self::Message>>) {
        let make_ls = get_layer_surface(SctkLayerSurfaceSettings {
            id: *WINDOW_ID,
            namespace: "launcher".into(),
            size_limits: Limits::NONE.min_width(120.).min_height(120.),
            keyboard_interactivity:
                cosmic::cctk::sctk::shell::wlr_layer::KeyboardInteractivity::OnDemand,
            ..Default::default()
        });

        let (send, recv) = channel(1);

        let desktop_entries: Vec<&'static DesktopEntry> = crate::xdg::find_desktop_entries()
            .into_iter()
            .map(|v| &*Box::leak(Box::new(v)))
            .collect();

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let nucleo = Nucleo::new(
            config,
            Arc::new(move || {
                let _ = send.try_send(());
            }),
            None,
            1,
        );
        let injector = nucleo.injector();
        for de in &desktop_entries {
            injector.push(
                *de,
                |de: &&'static DesktopEntry, col: &mut [Utf32String]| col[0] = de.name().into(),
            );
        }

        let task = Task::stream(nucleo_listener(recv));

        (
            AppModel {
                core,
                search_input: String::new(),
                nucleo,
                desktop_entries,
                show_entries: vec![],
            },
            batch(vec![make_ls, task]),
        )
    }

    fn update(&mut self, message: Self::Message) -> cosmic::app::Task<Self::Message> {
        match message {
            Message::InputChanged(s) => {
                self.search_input = s;
                self.nucleo.pattern.reparse(
                    0,
                    self.search_input.as_str(),
                    CaseMatching::Smart,
                    Normalization::Never,
                    false,
                ); // TODO: append
                let status = self.nucleo.tick(0);
                if !status.running && status.changed {
                    // somehow, the worker finished immediately,
                    // so send a NucleoResult to process its change.
                    // normally, this never happens!
                    return Task::done(cosmic::app::Message::App(Message::NucleoResult));
                }
            }
            Message::NucleoResult => {
                self.nucleo.tick(10);
                let snapshot = self.nucleo.snapshot();
                self.show_entries = snapshot.matched_items(..).map(|i| *i.data).collect();
            }
        }

        Task::none()
    }

    fn view(&self) -> Element<Self::Message> {
        unimplemented!() // no main window!
    }

    fn view_window(&self, _id: Id) -> Element<Self::Message> {
        let input_field = cosmic::widget::search_input("search", &self.search_input)
            .on_input(Message::InputChanged)
            .on_paste(Message::InputChanged)
            .width(200);

        let eles = self.show_entries.iter().map(|entry| {
            let name = entry.name();
            button(name).into()
        });
        let scrollable = cosmic::widget::scrollable(column(eles)).height(200);

        let col = column![input_field, scrollable,];

        let container = container(col)
            .class(cosmic::style::Container::WindowBackground)
            .width(Fixed(200.))
            .height(Fixed(200.)); // TODO: rounded background?

        // cosmic::widget::autosize::autosize(container, AUTOSIZE_ID.clone()).into()
        // container
        //     .align_x(Horizontal::Center)
        //     .align_y(Vertical::Center)
        //     .into()

        autosize(container, AUTOSIZE_ID.clone()).into()
    }
}

fn nucleo_listener(mut recv: tokio::sync::mpsc::Receiver<()>) -> impl Stream<Item = Message> {
    stream::channel(100, |mut output| async move {
        loop {
            let Some(_) = recv.recv().await else {
                return;
            };

            output.send(Message::NucleoResult).await.unwrap();
        }
    })
}
