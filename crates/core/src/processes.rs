use anyhow::{bail, ensure, Result};
use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};
pub type Cancel = Arc<AtomicBool>;
pub fn cancel_token() -> Cancel {
    Arc::new(AtomicBool::new(false))
}
pub async fn cancelled(c: &Cancel) {
    while !c.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(40)).await
    }
}
pub async fn limited(mut input: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut input)
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= cap, "Output limit exceeded");
    Ok(bytes)
}
pub struct Tree {
    pid: i32,
}
impl Tree {
    pub fn spawn(cmd: &mut Command) -> Result<(Child, Self)> {
        cmd.kill_on_drop(true).process_group(0);
        let child = cmd.spawn()?;
        let tree = Self {
            pid: child.id().unwrap() as i32,
        };
        Ok((child, tree))
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
    }
}
pub async fn run(
    cmd: &mut Command,
    limit: usize,
    seconds: u64,
    cancel: &Cancel,
) -> Result<Vec<u8>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, _tree) = Tree::spawn(cmd)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let work = async {
        let wait = async {
            let status = child.wait().await?;
            ensure!(status.success(), "Process exited unsuccessfully");
            Ok(())
        };
        let (out, _, _) = tokio::try_join!(limited(stdout, limit), limited(stderr, 65536), wait)?;
        Ok(out)
    };
    tokio::select! {result=tokio::time::timeout(Duration::from_secs(seconds),work)=> result.map_err(|_|anyhow::anyhow!("Process timed out"))?,_=cancelled(cancel)=>bail!("Cancelled")}
}
