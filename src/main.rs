use cosmic::app::{Core, Task};
use cosmic::{widget, Application, Element};

fn main() {
    println!("Hello, world!");

    let settings = cosmic::app::Settings::default().size_limits(
        cosmic::iced::Limits::NONE
            .min_width(200.)
            .min_height(180.),
    );

    cosmic::app::run::<AppModel>(settings, ()).expect("error running cosmic app");
}

struct AppModel {
    core: Core,
}

impl Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = ();
    const APP_ID: &'static str = "";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        (AppModel {
            core,
        }, Task::none())
    }

    fn view(&self) -> Element<Self::Message> {
        widget::text::title1("test").into()
    }
}
