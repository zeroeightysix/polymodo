use bincode::error::DecodeError;
use bincode::{Decode, Encode};
use derive_more::{Display, Error, From};
use interprocess::local_socket::tokio::{prelude::*, Listener, RecvHalf, SendHalf, Stream};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerNonblockingMode, ListenerOptions, Name, NameType,
    ToFsName, ToNsName,
};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

const POLYMODO_SOCK_PATH: &'static str = "/tmp/polymodo.sock";
const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

pub type IpcC2S = IpcClient<ClientboundMessage, ServerboundMessage>;
pub type IpcS2C = IpcClient<ServerboundMessage, ClientboundMessage>;

#[derive(Debug, Decode, Encode)]
pub enum ServerboundMessage {
    Ping,
}

#[derive(Debug, Decode, Encode)]
pub enum ClientboundMessage {
    Pong,
}

#[derive(Debug, Error, Display, From)]
pub enum IpcReceiveError {
    DecodeError(DecodeError),
    IoError(std::io::Error),
}

pub struct IpcClient<In, Out> {
    sender: Mutex<SendHalf>,
    receiver: Mutex<IpcClientReceiverInner>,
    marker: std::marker::PhantomData<(In, Out)>,
}

struct IpcClientReceiverInner {
    receiver: RecvHalf,
    buffer: Vec<u8>,
}

impl<In, Out> IpcClient<In, Out>
where
    In: bincode::Decode<()>,
    Out: bincode::Encode,
{
    fn new(stream: Stream) -> Self {
        let (receiver, sender) = stream.split();
        Self {
            receiver: Mutex::new(IpcClientReceiverInner {
                receiver,
                buffer: Vec::with_capacity(128),
            }),
            sender: Mutex::new(sender),
            marker: Default::default(),
        }
    }

    pub async fn send(&self, message: Out) -> anyhow::Result<()> {
        let mut sender = self.sender.lock().await;

        let bytes = bincode::encode_to_vec(message, BINCODE_CONFIG)?;
        let _ = sender.write(&bytes).await?;

        Ok(())
    }

    pub async fn recv(&self) -> Result<In, IpcReceiveError> {
        let IpcClientReceiverInner {
            ref mut receiver,
            ref mut buffer,
            ..
        } = &mut *self.receiver.lock().await;

        loop {
            match bincode::decode_from_slice(&buffer, BINCODE_CONFIG) {
                Ok((message, bytes)) => {
                    // remove `bytes` bytes from our buffer
                    // as we might have already read bytes of the next message, it's essential that
                    // we keep them around for the next attempt to `recv`!
                    drop(buffer.drain(..bytes));

                    return Ok(message);
                }
                Err(DecodeError::UnexpectedEnd { .. }) => {} // just read more!
                Err(e) => return Err(e.into()),
            }

            let _ = receiver.read_buf(buffer).await?;
        }
    }
}

pub struct IpcServer {
    pub listener: Listener,
}

impl IpcServer {
    pub async fn accept(
        &self,
    ) -> std::io::Result<IpcClient<ServerboundMessage, ClientboundMessage>> {
        let stream = self.listener.accept().await?;
        let client = IpcClient::new(stream);

        Ok(client)
    }
}

pub fn get_polymodo_socket_name() -> std::io::Result<Name<'static>> {
    if GenericNamespaced::is_supported() {
        "polymodo.sock".to_ns_name::<GenericNamespaced>()
    } else {
        POLYMODO_SOCK_PATH.to_fs_name::<GenericFilePath>()
    }
}

pub async fn create_ipc_server() -> std::io::Result<IpcServer> {
    let listener = create_connection_listener().await?;

    let server = IpcServer { listener };

    Ok(server)
}

async fn create_connection_listener() -> std::io::Result<Listener> {
    let name = get_polymodo_socket_name()?;
    let listener = try_creating_listener_with_cleanup(name)?;

    Ok(listener)
}

fn try_creating_listener_with_cleanup(name: Name) -> std::io::Result<Listener> {
    let create = |name: Name| -> std::io::Result<Listener> {
        ListenerOptions::new()
            .name(name)
            .nonblocking(ListenerNonblockingMode::Both)
            .create_tokio()
    };

    let listener = match create(name.clone()) {
        // `AddrInUse` signals that the socket (file) already exists,
        // yet we couldn't connect earlier (otherwise we wouldn't be trying to create a listener)
        // polymodo assumes in this instance that the socket is a corpse, tries to remove it if
        // possible, attempts to create again, and otherwise quits.
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let path = POLYMODO_SOCK_PATH;
            if name.is_path() {
                let path = PathBuf::from(path);
                if path.exists() && path.is_file() {
                    // try to delete the file
                    match std::fs::remove_file(&path) {
                        Ok(_) => {
                            log::info!("Removed (presumed) dead socket at {path:?}")
                        }
                        Err(e) => {
                            log::error!("Failed to remove socket at {path:?}, but neither can I connect to it.");
                            log::error!("This means polymodo cannot start as a daemon: either remove the socket file, or start polymodo in non-daemon mode.");
                            log::error!("Error: {e}");
                            std::process::exit(-1);
                        }
                    };
                }
            }

            // try creating the socket once more
            match create(name.clone()) {
                Ok(listener) => listener,
                Err(e) => {
                    log::error!("Could not create socket at {path}");
                    log::error!("This means polymodo cannot start as a daemon: either remove the socket file, or start polymodo in non-daemon mode.");
                    log::error!("Error: {e}");
                    std::process::exit(-1);
                }
            }
        }
        listener => listener?,
    };
    Ok(listener)
}

pub async fn connect_to_polymodo_daemon(
) -> std::io::Result<IpcC2S> {
    let name = get_polymodo_socket_name()?;
    let stream = Stream::connect(name).await?;

    let client = IpcClient::new(stream);

    Ok(client)
}
