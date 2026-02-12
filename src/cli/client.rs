use anyhow::{anyhow, Result};
use std::process;

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use clap::Parser;
use futures::FutureExt;
use multiaddr::Multiaddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use ww::peer_capnp;

const SHELL_WASM: &[u8] = include_bytes!("../../target/wasm32-wasip2/release/shell.wasm");

#[derive(Parser)]
#[command(name = "ww-cli")]
#[command(about = "Connect to a wetware node")]
struct Cli {
    /// Multiaddr of the node's RPC port (e.g. /ip4/127.0.0.1/tcp/2021)
    addr: String,

    /// Path to a WASM binary to execute. If omitted, spawns the embedded shell.
    wasm: Option<String>,
}

fn parse_tcp_multiaddr(addr_str: &str) -> Result<(String, u16)> {
    let addr: Multiaddr = addr_str.parse()?;
    let mut ip = None;
    let mut port = None;
    for proto in addr.iter() {
        match proto {
            multiaddr::Protocol::Ip4(a) => ip = Some(a.to_string()),
            multiaddr::Protocol::Ip6(a) => ip = Some(a.to_string()),
            multiaddr::Protocol::Tcp(p) => port = Some(p),
            _ => {}
        }
    }
    Ok((
        ip.ok_or_else(|| anyhow!("multiaddr missing IP component"))?,
        port.ok_or_else(|| anyhow!("multiaddr missing TCP port"))?,
    ))
}

async fn pump_stdin_to_bytestream(stream: peer_capnp::byte_stream::Client) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut buf = vec![0u8; 4096];
    loop {
        let n = stdin.read(&mut buf).await?;
        if n == 0 {
            stream.close_request().send().promise.await?;
            break;
        }
        let mut req = stream.write_request();
        req.get().set_data(&buf[..n]);
        req.send().promise.await?;
    }
    Ok(())
}

async fn pump_bytestream_to_stdout(stream: peer_capnp::byte_stream::Client) -> Result<()> {
    let mut stdout = tokio::io::stdout();
    loop {
        let mut req = stream.read_request();
        req.get().set_max_bytes(4096);
        let resp = req.send().promise.await?;
        let data = resp.get()?.get_data()?;
        if data.is_empty() {
            break;
        }
        stdout.write_all(data).await?;
        stdout.flush().await?;
    }
    Ok(())
}

async fn pump_bytestream_to_stderr(stream: peer_capnp::byte_stream::Client) -> Result<()> {
    let mut stderr = tokio::io::stderr();
    loop {
        let mut req = stream.read_request();
        req.get().set_max_bytes(4096);
        let resp = req.send().promise.await?;
        let data = resp.get()?.get_data()?;
        if data.is_empty() {
            break;
        }
        stderr.write_all(data).await?;
        stderr.flush().await?;
    }
    Ok(())
}

async fn wait_for_output_task(task: tokio::task::JoinHandle<Result<()>>) {
    let abort_handle = task.abort_handle();
    match timeout(Duration::from_secs(1), task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(err))) => eprintln!("stream pump failed: {err}"),
        Ok(Err(err)) => {
            if !err.is_cancelled() {
                eprintln!("stream pump join error: {err}");
            }
        }
        Err(_) => abort_handle.abort(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    ww::config::init_tracing();
    let cli = Cli::parse();

    let (ip, port) = parse_tcp_multiaddr(&cli.addr)?;
    let tcp = TcpStream::connect(format!("{ip}:{port}")).await?;
    let (reader, writer) = tokio::io::split(tcp);

    let rpc_network = VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        Side::Client,
        Default::default(),
    );
    let mut rpc_system = RpcSystem::new(Box::new(rpc_network), None);
    let host: peer_capnp::host::Client = rpc_system.bootstrap(Side::Server);

    let local = tokio::task::LocalSet::new();
    local.spawn_local(rpc_system.map(|_| ()));

    let exit_code = local
        .run_until(async {
            let executor = host.executor_request().send().pipeline.get_executor();

            let wasm_bytes = match &cli.wasm {
                Some(path) => std::fs::read(path)?,
                None => SHELL_WASM.to_vec(),
            };

            let mut request = executor.run_bytes_request();
            {
                let mut params = request.get();
                params.set_wasm(&wasm_bytes);
                params.reborrow().init_args(0);
                params.reborrow().init_env(0);
            }
            let response = request.send().promise.await?;
            let process = response.get()?.get_process()?;

            let stdin_resp = process.stdin_request().send().promise.await?;
            let proc_stdin = stdin_resp.get()?.get_stream()?;

            let stdout_resp = process.stdout_request().send().promise.await?;
            let proc_stdout = stdout_resp.get()?.get_stream()?;

            let stderr_resp = process.stderr_request().send().promise.await?;
            let proc_stderr = stderr_resp.get()?.get_stream()?;

            let stdin_task = tokio::task::spawn_local(pump_stdin_to_bytestream(proc_stdin));
            let stdout_task = tokio::task::spawn_local(pump_bytestream_to_stdout(proc_stdout));
            let stderr_task = tokio::task::spawn_local(pump_bytestream_to_stderr(proc_stderr));

            let wait_resp = process.wait_request().send().promise.await?;
            let exit_code = wait_resp.get()?.get_exit_code();

            stdin_task.abort();
            let _ = stdin_task.await;
            wait_for_output_task(stdout_task).await;
            wait_for_output_task(stderr_task).await;

            Ok::<i32, anyhow::Error>(exit_code)
        })
        .await?;

    process::exit(exit_code);
}
