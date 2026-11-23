//! Трек играет, пока докачивается: декодер читает то, что уже пришло, и ждёт остальное.

use std::io::{self, Read, Seek, SeekFrom};
#[cfg(target_os = "macos")]
use std::{fs::File, os::unix::fs::FileExt};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Сколько ждать следующих байтов, прежде чем считать загрузку оборвавшейся.
const STALL: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const MEMORY_LIMIT: usize = 8 << 20;

enum Storage {
    Memory(Vec<u8>),
    #[cfg(target_os = "macos")]
    Disk { file: File, len: u64 },
}

impl Default for Storage {
    fn default() -> Self {
        Self::Memory(Vec::new())
    }
}

impl Storage {
    fn with_length(length: Option<u64>) -> Self {
        let capacity = length.unwrap_or(0) as usize;
        #[cfg(target_os = "macos")]
        let capacity = if capacity > MEMORY_LIMIT {
            if let Ok(file) = tempfile::tempfile() {
                return Self::Disk { file, len: 0 };
            }
            MEMORY_LIMIT
        } else {
            capacity
        };
        Self::Memory(Vec::with_capacity(capacity))
    }

    fn len(&self) -> u64 {
        match self {
            Self::Memory(data) => data.len() as u64,
            #[cfg(target_os = "macos")]
            Self::Disk { len, .. } => *len,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Memory(data) => {
                #[cfg(target_os = "macos")]
                if data.len() <= MEMORY_LIMIT && bytes.len() > MEMORY_LIMIT - data.len() {
                    if let Ok(file) = tempfile::tempfile() {
                        let offset = data.len() as u64;
                        if file.write_all_at(data, 0).and_then(|()| file.write_all_at(bytes, offset)).is_ok() {
                            *self = Self::Disk { file, len: offset + bytes.len() as u64 };
                            return Ok(());
                        }
                    }
                }
                data.extend_from_slice(bytes);
            }
            #[cfg(target_os = "macos")]
            Self::Disk { file, len } => {
                file.write_all_at(bytes, *len)?;
                *len += bytes.len() as u64;
            }
        }
        Ok(())
    }

    fn read_at(&self, position: u64, out: &mut [u8]) -> io::Result<usize> {
        let len = out.len().min((self.len() - position) as usize);
        match self {
            Self::Memory(data) => {
                let from = position as usize;
                out[..len].copy_from_slice(&data[from..from + len]);
                Ok(len)
            }
            #[cfg(target_os = "macos")]
            Self::Disk { file, .. } => file.read_at(&mut out[..len], position),
        }
    }
}

#[derive(Default)]
struct State {
    data: Storage,
    len: Option<u64>,
    done: bool,
    err: Option<String>,
}

#[derive(Default)]
pub struct Buf {
    state: Mutex<State>,
    cv: Condvar,
}

impl Buf {
    /// Качает ответ в фоне. Если буфер больше никому не нужен (трек пропустили), загрузка
    /// останавливается.
    pub fn download(mut resp: reqwest::blocking::Response) -> Arc<Buf> {
        let buf = Arc::new(Buf::default());
        {
            let mut st = buf.state.lock().unwrap();
            st.len = resp.content_length();
            st.data = Storage::with_length(st.len);
        }
        let b = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                if Arc::strong_count(&b) == 1 {
                    return;
                }
                let res = resp.read(&mut chunk);
                let mut st = b.state.lock().unwrap();
                match res.and_then(|n| st.data.append(&chunk[..n]).map(|()| n)) {
                    Ok(0) => st.done = true,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        st.err = Some(format!("сеть: {e}"));
                        st.done = true;
                    }
                }
                let done = st.done;
                drop(st);
                b.cv.notify_all();
                if done {
                    return;
                }
            }
        });
        buf
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn completed_bytes(bytes: Vec<u8>) -> Arc<Self> {
        let state = State { len: Some(bytes.len() as u64), done: true, data: Storage::Memory(bytes), err: None };
        Arc::new(Self { state: Mutex::new(state), cv: Condvar::new() })
    }

    pub fn complete(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.done && state.err.is_none()
    }

    pub fn reader(self: &Arc<Self>) -> Reader {
        Reader { buf: self.clone(), pos: 0 }
    }

    /// Ждёт, пока придёт больше `pos` байтов или загрузка закончится.
    fn wait_past(&self, pos: u64) -> io::Result<MutexGuard<'_, State>> {
        let mut st = self.state.lock().unwrap();
        while st.data.len() <= pos && !st.done {
            let (next, t) = self.cv.wait_timeout(st, STALL).unwrap();
            st = next;
            if t.timed_out() && st.data.len() <= pos && !st.done {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "трек перестал загружаться"));
            }
        }
        Ok(st)
    }
}

pub struct Reader {
    buf: Arc<Buf>,
    pos: u64,
}

impl symphonia::core::io::MediaSource for Reader {
    fn is_seekable(&self) -> bool { self.byte_len().is_some() }

    fn byte_len(&self) -> Option<u64> {
        let state = self.buf.state.lock().unwrap();
        if state.done { Some(state.data.len()) } else { state.len }
    }
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let st = self.buf.wait_past(self.pos)?;
        let have = st.data.len();
        if self.pos >= have {
            return match &st.err {
                Some(e) => Err(io::Error::other(e.clone())),
                None => Ok(0),
            };
        }
        let n = st.data.read_at(self.pos, out)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => {
                let len = {
                    let st = self.buf.state.lock().unwrap();
                    st.len
                };
                let len = match len {
                    Some(n) => n,
                    None => self.buf.wait_past(u64::MAX)?.data.len(),
                };
                len.checked_add_signed(d)
            }
        };
        self.pos = pos.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "позиция до начала файла"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_waits_for_data() {
        let buf = Arc::new(Buf::default());
        let mut r = buf.reader();
        let b = buf.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            b.state.lock().unwrap().data.append(b"abc").unwrap();
            b.cv.notify_all();
            std::thread::sleep(Duration::from_millis(50));
            let mut st = b.state.lock().unwrap();
            st.data.append(b"de").unwrap();
            st.done = true;
            drop(st);
            b.cv.notify_all();
        });
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        t.join().unwrap();
        assert_eq!(all, b"abcde");
        assert_eq!(r.seek(SeekFrom::End(-2)).unwrap(), 3);
        let mut tail = String::new();
        r.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "de");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn long_stream_keeps_seekable_data_on_disk() {
        let mut data = Storage::default();
        let chunk = [b'a'; 64 * 1024];
        for _ in 0..MEMORY_LIMIT / chunk.len() {
            data.append(&chunk).unwrap();
        }
        data.append(b"bcd").unwrap();
        assert!(matches!(data, Storage::Disk { .. }));
        let mut tail = [0; 4];
        assert_eq!(data.read_at(MEMORY_LIMIT as u64 - 1, &mut tail).unwrap(), 4);
        assert_eq!(&tail, b"abcd");
    }
}
