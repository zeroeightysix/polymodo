use crate::windowing::app::App;
use crate::windowing::client::Client;
use crate::windowing::surface::{FullSurfaceId, LayerSurfaceOptions, SurfaceId};
use crate::windowing::windowing::DispatcherRequest;
use slotmap::{new_key_type, SlotMap};
use smithay_client_toolkit::shell;
use tokio::select;
use tokio::sync::mpsc;

new_key_type! {
    struct AppKey;
}

type AppData = (Vec<FullSurfaceId>, Box<dyn App>, egui::Context);

struct Polymodo {
    client: Client,
    apps: SlotMap<AppKey, AppData>,
    sender: mpsc::Sender<DispatcherRequest>,
}

impl Polymodo {
    fn paint(
        client: &mut Client,
        surfs: &mut Vec<FullSurfaceId>,
        app: &mut Box<dyn App>,
        ctx: &mut egui::Context,
    ) {
        for surf in surfs {
            client.repaint_surface(surf.surface_id.clone(), ctx, |ctx: &egui::Context| {
                app.render(ctx);
            });
        }
    }

    fn repaint_surf(&mut self, surface_id: SurfaceId) {
        if let Some((_, (surfs, app, ctx))) = self
            .apps
            .iter_mut()
            .find(|(_, (surfs, _, _))| surfs.iter().any(|id| id.surface_id == surface_id))
        {
            Self::paint(&mut self.client, surfs, app, ctx);
        }
    }

    fn repaint_view(&mut self, viewport_id: egui::ViewportId) {
        if let Some((_, (surfs, app, ctx))) = self
            .apps
            .iter_mut()
            .find(|(_, (surfs, _, _))| surfs.iter().any(|id| id.viewport_id == viewport_id))
        {
            Self::paint(&mut self.client, surfs, app, ctx);
        }
    }

    fn repaint_app(&mut self, app: AppKey) {
        if let Some((surfs, app, ctx)) = self.apps.get_mut(app) {
            Self::paint(&mut self.client, surfs, app, ctx);
        }
    }

    fn create_context(&self) -> egui::Context {
        let sender = self.sender.clone();
        let ctx = egui::Context::default();
        ctx.set_request_repaint_callback(move |info| {
            // TODO: delay
            let _ = sender.try_send(DispatcherRequest::RepaintViewport(
                info.viewport_id,
                info.current_cumulative_pass_nr,
            ));
        });
        ctx
    }
}

pub struct AppSender {
    for_app: AppKey,
    send: mpsc::Sender<(AppKey, AppRequest)>,
}

impl AppSender {
    fn new(for_app: AppKey, send: mpsc::Sender<(AppKey, AppRequest)>) -> Self {
        Self { for_app, send }
    }

    pub fn send(&self, request: AppRequest) {
        match self.send.blocking_send((self.for_app, request)) {
            Ok(_) => {}
            Err(mpsc::error::SendError((key, v))) => {
                log::error!(
                    "failed to send app request, so '{v:?}' was dropped for app with key {key:?}"
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppRequest {
    Repaint,
    Exit(Response),
}

type Response = ();

pub async fn run() -> anyhow::Result<()> {
    let (dispatch_sender, mut dispatch_recv) = mpsc::channel::<DispatcherRequest>(128);

    // let app = App::create(send.clone());
    // let search_notify = app.search.notify();
    //
    // tokio::spawn(async move {
    //     loop {
    //         search_notify.notified().await;
    //         let _ = send.send(Message::Search).await;
    //     }
    // });

    let options = LayerSurfaceOptions {
        anchor: shell::wlr_layer::Anchor::empty(),
        width: 350,
        height: 400,
        ..Default::default()
    };

    log::trace!("connect to wayland");
    let client = Client::create(Default::default(), dispatch_sender.clone()).await?;

    let mut poly = Polymodo {
        client,
        apps: SlotMap::with_key(),
        sender: dispatch_sender,
    };

    let launch = crate::mode::launch::Launcher::create();
    let surf = poly
        .client
        .create_surface(egui::ViewportId::ROOT, options)
        .await?;
    poly.apps
        .insert((vec![surf], Box::new(launch), poly.create_context()));

    let mut pass_counter = 0;

    log::trace!("enter event loop");
    loop {
        select! {
            result = poly.client.update() => {
                let () = result?;
            }
            Some(message) = dispatch_recv.recv() => {
                match message {
                    DispatcherRequest::RepaintSurface(surf) => {
                        log::trace!("repaint (surf) {surf:?}");
                        poly.repaint_surf(surf);
                    }
                    DispatcherRequest::RepaintViewport(viewport_id, new_pass) => {
                        log::trace!("repaint (view) {viewport_id:?}");
                        if new_pass > pass_counter {
                            pass_counter = new_pass;
                            poly.repaint_view(viewport_id);
                        }
                    }
                }
                // TODO: client.app().on_message(message);
            }
        }
    }
}
