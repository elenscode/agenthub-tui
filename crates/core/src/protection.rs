#[cfg(not(windows))]
use anyhow::bail;
#[cfg(windows)]
use anyhow::ensure;
use anyhow::Result;
pub trait Protection {
    fn seal(&self, bytes: &[u8]) -> Result<Vec<u8>>;
    fn open(&self, bytes: &[u8]) -> Result<Vec<u8>>;
}
pub struct UserProtection;
impl Protection for UserProtection {
    fn seal(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        dpapi(bytes, true)
    }
    fn open(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        dpapi(bytes, false)
    }
}
#[cfg(not(windows))]
fn dpapi(_: &[u8], _: bool) -> Result<Vec<u8>> {
    bail!("Protected writes require the Windows host (current-user DPAPI)")
}
#[cfg(windows)]
fn dpapi(bytes: &[u8], seal: bool) -> Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr() as _,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    unsafe {
        let ok = if seal {
            CryptProtectData(
                &input,
                null(),
                null(),
                null_mut(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null_mut(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        ensure!(ok != 0, "Windows user protection failed");
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(result)
    }
}
