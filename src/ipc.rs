use crate::app::AppName;
use bincode::error::DecodeError;
use bincode::{Decode, Encode};
use derive_more::{Display, Error, From};
use smol::io::{AsyncReadExt, AsyncWriteExt};
use smol::lock::Mutex;
use smol::net::unix::{UnixListener, UnixStream};
use smol::Async;
use std::net::Shutdown;
use std::os::unix::net::SocketAddr;
use std::sync::Arc;

const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

pub type IpcC2S = IpcClient<ClientboundMessage, ServerboundMessage>;
pub type IpcS2C = IpcClient<ServerboundMessage, ClientboundMessage>;

#[derive(Debug, Decode, Encode)]
pub enum ServerboundMessage {
    Ping,
    Spawn(AppSpawnOptions),
    Goodbye,
}

#[derive(Debug, Decode, Encode)]
pub struct AppSpawnOptions {
    pub app_name: AppName,
    pub single: bool,
}

#[derive(Debug, Decode, Encode)]
pub enum ClientboundMessage {
    Pong,
    /// Yes/no, an app with that type name is already running
    Running(String, bool),
    AppResult(String), // TODO: apps return much prettier things than String. This could be type-safe, but requires a bit of thought.
}

#[derive(Debug, Error, Display, From)]
pub enum IpcReceiveError {
    DecodeError(DecodeError),
    IoError(std::io::Error),
}

pub struct IpcClient<In, Out> {
    stream: UnixStream,
    buffer: Arc<Mutex<Vec<u8>>>,
    addr: SocketAddr,
    marker: std::marker::PhantomData<(In, Out)>,
}

impl<A, B> IpcClient<A, B> {
    fn new(stream: UnixStream, addr: SocketAddr) -> Self {
        Self {
            stream,
            buffer: Default::default(),
            addr,
            marker: Default::default(),
        }
    }

    pub fn addr(&self) -> &SocketAddr {
        &self.addr
    }

    pub async fn shutdown(&self) -> std::io::Result<()> {
        self.stream.shutdown(Shutdown::Write)?;

        Ok(())
    }
}

impl<In, Out> IpcClient<In, Out>
where
    In: bincode::Decode<()>,
    Out: bincode::Encode,
{
    pub async fn send(&self, message: Out) -> anyhow::Result<()> {
        let mut stream = self.stream.clone();

        let bytes = bincode::encode_to_vec(message, BINCODE_CONFIG)?;
        let _ = stream.write(&bytes).await?;

        Ok(())
    }

    pub async fn recv(&self) -> Result<In, IpcReceiveError> {
        loop {
            let mut buffer = self.buffer.lock().await;

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

            let mut stream = self.stream.clone();

            if stream.read(&mut buffer).await? == 0 {
                let err: std::io::Error = std::io::ErrorKind::BrokenPipe.into();
                return Err(err.into());
            }
        }
    }
}

impl<A, B> Clone for IpcClient<A, B> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            buffer: Arc::clone(&self.buffer),
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
        let client = IpcClient::new(stream, addr);

        Ok(client)
    }
}

pub fn get_polymodo_socket_addr() -> SocketAddr {
    use std::os::linux::net::SocketAddrExt;

    SocketAddr::from_abstract_name(b"polymodo.sock")
        .expect("can't construct polymodo socket address. Is abstract namespacing not supported on the version of linux you are running?")
}

pub fn create_ipc_server() -> std::io::Result<IpcServer> {
    let listener = create_listener()?;

    let server = IpcServer { listener };

    Ok(server)
}

fn create_listener() -> std::io::Result<UnixListener> {
    let addr = get_polymodo_socket_addr();
    let listener = bind_listener(addr)?;

    Ok(listener)
}

fn bind_listener(addr: SocketAddr) -> std::io::Result<UnixListener> {
    let listener = std::os::unix::net::UnixListener::bind_addr(&addr)?;
    listener.set_nonblocking(true)?;

    let async_listener = Async::new(listener)?;

    Ok(async_listener.into())
}

pub fn connect_to_polymodo_daemon() -> std::io::Result<IpcC2S> {
    let addr = get_polymodo_socket_addr();
    let stream = std::os::unix::net::UnixStream::connect_addr(&addr)?;
    stream.set_nonblocking(true)?;
    let stream = stream.try_into()?;

    let client = IpcClient::new(stream, addr);

    Ok(client)
}
