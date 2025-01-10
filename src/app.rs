use cosmic::app::{Core, Settings};
use cosmic::iced::window::Id;
use cosmic::iced_runtime::core::window::Id as SurfaceId;
use cosmic::iced_runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced_winit::commands::layer_surface::get_layer_surface;
use cosmic::{Application, Element, Task};
use std::sync::LazyLock;
use cosmic::iced::Limits;
use cosmic::iced_widget::button;

static WINDOW_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);
static AUTOSIZE_ID: LazyLock<cosmic::iced::id::Id> = LazyLock::new(|| cosmic::iced::id::Id::new("autosize"));

pub fn run() -> cosmic::iced::Result {
    cosmic::app::run::<AppModel>(
        Settings::default()
            .antialiasing(true)
            .client_decorations(true)
            .debug(false)
            .default_text_size(16.0)
            .no_main_window(true)
            .exit_on_close(false),
        (),
    )?;

    Ok(())
}

#[derive(Debug, Clone)]
enum Message {
    HelloWorld,
}

struct AppModel {
    core: Core,
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
            size_limits: Limits::NONE
                .min_width(120.)
                .min_height(120.),
            ..Default::default()
        });

        (AppModel {
            core,
        }, make_ls)
    }

    fn update(&mut self, _message: Self::Message) -> cosmic::app::Task<Self::Message> {
        println!("hello world!");

        Task::none()
    }

    fn view(&self) -> Element<Self::Message> {
        unimplemented!() // no main window!
    }

    fn view_window(&self, _id: Id) -> Element<Self::Message> {
        let button = button("Hello world")
            .on_press(Message::HelloWorld);

        cosmic::widget::autosize::autosize(button, AUTOSIZE_ID.clone())
            .into()
    }
}
