//! Small OS helpers the transfer engine needs: free space and positional reads/writes,
//! so several connections can fill different parts of one file at once.

use std::{fs::File, io, path::Path};

/// Bytes the current user may still write on the volume holding `dir` (which must exist).
pub fn available_space(dir: &Path) -> io::Result<u64> {
    imp::available_space(dir)
}

/// Writes all of `buf` at `offset`, without moving any shared file cursor.
pub fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = imp::write_at(file, buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Fills `buf` from `offset`; fails if the file ends first (it shrank while sending).
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match imp::read_at(file, buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
mod imp {
    use std::{
        ffi::CString, fs::File, io, os::unix::ffi::OsStrExt, os::unix::fs::FileExt, path::Path,
    };

    pub fn available_space(dir: &Path) -> io::Result<u64> {
        let path = CString::new(dir.as_os_str().as_bytes())?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `st` is a valid out-pointer.
        if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }

    pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.write_at(buf, offset)
    }

    pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        file.read_at(buf, offset)
    }
}

#[cfg(windows)]
mod imp {
    use std::{fs::File, io, os::windows::ffi::OsStrExt, os::windows::fs::FileExt, path::Path};

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    pub fn available_space(dir: &Path) -> io::Result<u64> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated; the other out-pointers may be null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(free)
    }

    // seek_* also move the handle's cursor; every caller passes an explicit offset, so that is harmless.
    pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.seek_write(buf, offset)
    }

    pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        file.seek_read(buf, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_io_and_free_space() {
        let dir = std::env::temp_dir().join(format!("rustdrop-disk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(available_space(&dir).unwrap() > 0);

        let path = dir.join("f");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(10).unwrap();
        write_all_at(&file, b"world", 5).unwrap();
        write_all_at(&file, b"hello", 0).unwrap();
        let mut buf = [0u8; 10];
        read_exact_at(&file, &mut buf, 0).unwrap();
        assert_eq!(&buf, b"helloworld");
        assert!(read_exact_at(&file, &mut buf, 5).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
