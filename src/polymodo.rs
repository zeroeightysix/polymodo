use crate::app_surface_driver;
use crate::app_surface_driver::{create_app_driver, new_app_key, AppEvent};
use crate::mode::launch::Launcher;
use crate::windowing::app::{App, AppSender, AppSetup};
use crate::windowing::client::{SurfaceEvent, WaylandClient};
use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub async fn run() -> anyhow::Result<std::convert::Infallible> {
    // two channels: one for events (that is Send + Sync)
    let (surf_driver_event_sender, event_receive) = mpsc::channel(128);
    // and one for app creation, which is !Send and !Sync, so that we do not need a Send+Sync requirement
    // on App implementations. This allows apps to use normal sync and memory sharing utils, like Rc.
    let (surf_driver_app_sender, app_receive) = local_channel::mpsc::channel();

    // set up the connection to wayland and wgpu
    let client = WaylandClient::create(surf_driver_event_sender.clone()).await?;
    let surface_setup = client.new_surface_setup(Default::default()).await?;

    // set up the surface app driver task.
    // this one processes events coming from the dispatcher,
    // render requests from the dispatcher and other sources,
    // and holds app state.
    let _surf_drive_task = app_surface_driver::create_surface_driver_task(
        surf_driver_event_sender.clone(),
        event_receive,
        app_receive,
        surface_setup,
    );

    // set up the dispatch task which polls wayland and sends client to the surface app driver
    let dispatch_task = create_dispatch_task(client);

    spawn_app::<Launcher>(surf_driver_event_sender, surf_driver_app_sender)?;

    // both surf_drive_task and dispatch_task should never complete.
    // we could join and wait on them here, but either will never finish.. so we just pick one:
    Ok(dispatch_task.await?)
}

fn spawn_app<A: App + 'static>(
    surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
    surf_driver_app_sender: local_channel::mpsc::Sender<AppEvent>,
) -> anyhow::Result<JoinHandle<<A as App>::Output>>
where
    A::Message: 'static,
    A::Output: 'static,
{
    // create a new key for this app.
    // (it's just a number)
    let key = new_app_key();
    let send = AppSender::new(key, surf_driver_app_sender.clone());
    let AppSetup { app, mut effects } = A::create(send);
    let driver = create_app_driver(key, app, surf_driver_event_sender);

    surf_driver_app_sender
        .send(AppEvent::NewApp {
            app_driver: Box::new(driver),
            layer_surface_options: Launcher::layer_surface_options(),
        })
        .ok()
        .context("Failed to spawn launcher app")?;

    Ok(tokio::task::spawn_local(async move {
        effects.join_next().await.unwrap().unwrap() // TODO: we need an abstraction on AppSetup to guarantee an effect
    }))
}

fn create_dispatch_task(mut client: WaylandClient) -> JoinHandle<std::convert::Infallible> {
    tokio::task::spawn_blocking(move || loop {
        if let Err(e) = client.dispatch() {
            log::warn!("error dispatching: {e}");
            std::process::exit(1);
        }
    })
}
