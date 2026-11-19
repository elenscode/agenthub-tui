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
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
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
    #[cfg(unix)]
    pid: i32,
    #[cfg(windows)]
    job: isize,
}
impl Tree {
    pub fn spawn(cmd: &mut Command) -> Result<(Child, Self)> {
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        #[cfg(unix)]
        {
            let tree = Self {
                pid: child.id().unwrap() as i32,
            };
            Ok((child, tree))
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                ensure!(!h.is_null(), "Job creation failed");
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as _,
                    std::mem::size_of_val(&info) as u32,
                ) == 0
                    || AssignProcessToJobObject(h, child.raw_handle().unwrap() as _) == 0
                {
                    CloseHandle(h);
                    bail!("Process isolation failed")
                }
                Ok((child, Self { job: h as isize }))
            }
        }
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job as _);
        }
    }
}
pub async fn run(
    cmd: &mut Command,
    input: Vec<u8>,
    limit: usize,
    seconds: u64,
    cancel: &Cancel,
) -> Result<Vec<u8>> {
    capture(cmd, input, limit, seconds, cancel, false).await
}
pub async fn rpc(
    cmd: &mut Command,
    input: Vec<u8>,
    limit: usize,
    seconds: u64,
    cancel: &Cancel,
) -> Result<Vec<u8>> {
    capture(cmd, input, limit, seconds, cancel, true).await
}
async fn capture(
    cmd: &mut Command,
    input: Vec<u8>,
    limit: usize,
    seconds: u64,
    cancel: &Cancel,
    hold: bool,
) -> Result<Vec<u8>> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, _tree) = Tree::spawn(cmd)?;
    let (mut stdin, stdout, stderr) = (
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        child.stderr.take().unwrap(),
    );
    let work = async {
        let send = async move {
            stdin.write_all(&input).await?;
            if !hold {
                stdin.shutdown().await?;
            }
            Ok::<_, anyhow::Error>(if hold { Some(stdin) } else { None })
        };
        let wait = async {
            let status = child.wait().await?;
            ensure!(status.success(), "Process exited unsuccessfully");
            Ok(())
        };
        let (_, out, _, _) =
            tokio::try_join!(send, limited(stdout, limit), limited(stderr, 65536), wait)?;
        Ok(out)
    };
    tokio::select! {result=tokio::time::timeout(Duration::from_secs(seconds),work)=> result.map_err(|_|anyhow::anyhow!("Process timed out"))?,_=cancelled(cancel)=>bail!("Cancelled")}
}
