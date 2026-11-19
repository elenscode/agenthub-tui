use agenthub_core::{
    model::Context,
    processes::cancel_token,
    runtime::{self, Request, LIMIT},
};
use anyhow::{ensure, Result};
use std::io::{BufRead, BufReader, Read, Write};
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut input = BufReader::new(std::io::stdin());
    let mut line = vec![];
    (&mut input)
        .take(LIMIT as u64 + 1)
        .read_until(b'\n', &mut line)?;
    ensure!(line.len() <= LIMIT, "Request limit exceeded");
    let mut req: Request = serde_json::from_slice(&line)?;
    #[cfg(unix)]
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "Helper requires a non-root user"
    );
    let env = req.context.environment.clone();
    let mut local = Context::local(&req.context.project)?;
    local.environment = env;
    req.context = local;
    let token = cancel_token();
    let work = runtime::serve(req, &token);
    let reply = if std::env::args().nth(1).as_deref() == Some("--leased") {
        // A detached reader lets a completed request exit while the host still holds stdin open.
        let (closed, disconnected) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = input.read(&mut [0]);
            let _ = closed.send(());
        });
        tokio::select! {r=work=>r,_=disconnected=>return Ok(())}
    } else {
        work.await
    };
    let bytes = serde_json::to_vec(&reply)?;
    ensure!(bytes.len() <= LIMIT, "Reply limit exceeded");
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
