//! Трек играет, пока докачивается: декодер читает то, что уже пришло, и ждёт остальное.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Сколько ждать следующих байтов, прежде чем считать загрузку оборвавшейся.
const STALL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct State {
    data: Vec<u8>,
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
            if let Some(n) = st.len {
                st.data.reserve_exact(n as usize);
            }
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
                match res {
                    Ok(0) => st.done = true,
                    Ok(n) => st.data.extend_from_slice(&chunk[..n]),
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

    pub fn reader(self: &Arc<Self>) -> Reader {
        Reader { buf: self.clone(), pos: 0 }
    }

    /// Ждёт, пока придёт больше `pos` байтов или загрузка закончится.
    fn wait_past(&self, pos: u64) -> io::Result<MutexGuard<'_, State>> {
        let mut st = self.state.lock().unwrap();
        while st.data.len() as u64 <= pos && !st.done {
            let (next, t) = self.cv.wait_timeout(st, STALL).unwrap();
            st = next;
            if t.timed_out() && st.data.len() as u64 <= pos && !st.done {
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

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let st = self.buf.wait_past(self.pos)?;
        let have = st.data.len() as u64;
        if self.pos >= have {
            return match &st.err {
                Some(e) => Err(io::Error::other(e.clone())),
                None => Ok(0),
            };
        }
        let n = out.len().min((have - self.pos) as usize);
        let from = self.pos as usize;
        out[..n].copy_from_slice(&st.data[from..from + n]);
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
                    None => self.buf.wait_past(u64::MAX)?.data.len() as u64,
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
            b.state.lock().unwrap().data.extend_from_slice(b"abc");
            b.cv.notify_all();
            std::thread::sleep(Duration::from_millis(50));
            let mut st = b.state.lock().unwrap();
            st.data.extend_from_slice(b"de");
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
}
