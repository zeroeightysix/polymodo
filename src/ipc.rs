use bincode::error::DecodeError;
use bincode::{Decode, Encode};
use derive_more::{Display, Error, From};
use std::os::unix::net::SocketAddr;
use std::rc::Rc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

pub type IpcC2S = IpcClient<ClientboundMessage, ServerboundMessage>;
pub type IpcS2C = IpcClient<ServerboundMessage, ClientboundMessage>;

#[derive(Debug, Decode, Encode)]
pub enum ServerboundMessage {
    Ping,
    Spawn(AppDescription),
}

#[derive(Debug, Decode, Encode)]
pub enum AppDescription {
    Launcher,
}

#[derive(Debug, Decode, Encode)]
pub enum ClientboundMessage {
    Pong,
    AppResult(String), // TODO: apps return much prettier things than String. This could be type-safe, but requires a bit of thought.
}

#[derive(Debug, Error, Display, From)]
pub enum IpcReceiveError {
    DecodeError(DecodeError),
    IoError(std::io::Error),
}

pub struct IpcClient<In, Out> {
    sender: Rc<Mutex<OwnedWriteHalf>>,
    receiver: Rc<Mutex<IpcClientReceiverInner>>,
    addr: SocketAddr,
    marker: std::marker::PhantomData<(In, Out)>,
}

struct IpcClientReceiverInner {
    receiver: OwnedReadHalf,
    buffer: Vec<u8>,
}

impl<In, Out> IpcClient<In, Out>
where
    In: bincode::Decode<()>,
    Out: bincode::Encode,
{
    fn new(stream: UnixStream, addr: SocketAddr) -> Self {
        let (receiver, sender) = stream.into_split();
        Self {
            receiver: Rc::new(Mutex::new(IpcClientReceiverInner {
                receiver,
                buffer: Vec::with_capacity(128),
            })),
            sender: Rc::new(Mutex::new(sender)),
            marker: Default::default(),
            addr,
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

            if receiver.read_buf(buffer).await? == 0 {
                let err: std::io::Error = std::io::ErrorKind::BrokenPipe.into();
                return Err(err.into());
            }
        }
    }
}

impl<A, B> Clone for IpcClient<A, B> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            receiver: self.receiver.clone(),
            addr: self.addr.clone(),
            marker: Default::default(),
        }
    }
}

pub struct IpcServer {
    pub listener: UnixListener,
}

impl IpcServer {
    pub async fn accept(
        &self,
    ) -> std::io::Result<IpcClient<ServerboundMessage, ClientboundMessage>> {
        let (stream, addr) = self.listener.accept().await?;
        let client = IpcClient::new(stream, addr.into());

        Ok(client)
    }
}

pub fn get_polymodo_socket_addr() -> SocketAddr {
    use std::os::linux::net::SocketAddrExt;

    SocketAddr::from_abstract_name(b"polymodo.sock")
        .expect("can't construct polymodo socket address. Is abstract namespacing not supported on the version of linux you are running?")
}

pub async fn create_ipc_server() -> std::io::Result<IpcServer> {
    let listener = create_listener().await?;

    let server = IpcServer { listener };

    Ok(server)
}

async fn create_listener() -> std::io::Result<UnixListener> {
    let addr = get_polymodo_socket_addr();
    let listener = bind_listener(addr)?;

    Ok(listener)
}

fn bind_listener(addr: SocketAddr) -> std::io::Result<UnixListener> {
    let listener = std::os::unix::net::UnixListener::bind_addr(&addr)?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;

    Ok(listener)
}

pub async fn connect_to_polymodo_daemon() -> std::io::Result<IpcC2S> {
    let addr = get_polymodo_socket_addr();
    let stream = std::os::unix::net::UnixStream::connect_addr(&addr)?;
    stream.set_nonblocking(true)?;
    let stream = stream.try_into()?;

    let client = IpcClient::new(stream, addr.into());

    Ok(client)
}
