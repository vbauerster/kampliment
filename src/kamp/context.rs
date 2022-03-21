use crossbeam_channel::Sender;
use std::borrow::Cow;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::thread;

use super::error::Error;
use super::kak;

const KAKOUNE_SESSION: &str = "KAKOUNE_SESSION";
const KAKOUNE_CLIENT: &str = "KAKOUNE_CLIENT";
const END_TOKEN: &str = "<<END>>";

#[derive(Debug)]
pub(crate) struct Context<'a> {
    pub session: String,
    pub client: Option<String>,
    out_path: Cow<'a, Path>,
}

impl Context<'_> {
    pub fn new(session: String, client: Option<String>) -> Self {
        let mut path = std::env::temp_dir();
        path.push(session.clone() + "-kamp");
        Context {
            session,
            client,
            out_path: Cow::from(path),
        }
    }
    pub fn from_env(client: Option<String>) -> Option<Self> {
        use std::env::var;
        var(KAKOUNE_SESSION)
            .map(|s| Context::new(s, client.or_else(|| var(KAKOUNE_CLIENT).ok())))
            .ok()
    }
    pub fn send(&self, body: &str, buffer: Option<String>) -> Result<String, Error> {
        let kak_jh = thread::spawn({
            let mut cmd = String::from("try %{ eval");
            if let Some(buffer) = buffer.as_deref() {
                cmd.push_str(" -buffer ");
                cmd.push_str(buffer);
            } else if let Some(client) = self.client.as_deref() {
                cmd.push_str(" -client ");
                cmd.push_str(client);
            }
            cmd.push_str(" %{\n");
            cmd.push_str(body);
            cmd.push_str("}} catch %{\n");
            cmd.push_str("  echo -debug kamp: %val{error}\n");
            cmd.push_str("  echo -to-file %opt{kamp_err} %val{error}\n");
            cmd.push_str("}\n");
            cmd.push_str("echo -to-file %opt{kamp_out} ");
            cmd.push_str(END_TOKEN);

            eprintln!("send: {}", cmd);

            let session = self.session.clone();
            move || kak::pipe(&session, &cmd)
        });

        let (s0, r) = crossbeam_channel::bounded(1);
        let s1 = s0.clone();
        let out_jh = read_out(self.get_out_path(false), s0);
        let err_jh = read_err(self.get_out_path(true), s1);

        let res = r.recv().map_err(anyhow::Error::new)?;
        let jh = if res.is_err() { err_jh } else { out_jh };
        jh.join()
            .unwrap()
            .and(kak_jh.join().expect("couldn't join kak thread"))
            .and(res)
    }

    pub fn connect(&self, body: &str) -> Result<(), Error> {
        let kak_jh = thread::spawn({
            let session = self.session.clone();
            let mut cmd = String::from("try %{ eval -try-client '' %{\n");
            cmd.push_str(body);
            cmd.push_str("}} catch %{\n");
            cmd.push_str("  echo -debug kamp: %val{error}\n");
            cmd.push_str("  echo -to-file %opt{kamp_err} %val{error}\n");
            cmd.push_str("  quit 1\n");
            cmd.push_str("}");
            eprintln!("connect: {}", cmd);
            move || kak::connect(&session, &cmd)
        });

        let (s0, r) = crossbeam_channel::bounded(0);
        let s1 = s0.clone();
        let out_jh = read_out(self.get_out_path(false), s0);
        let err_jh = read_err(self.get_out_path(true), s1);

        for (i, res) in r.iter().enumerate() {
            match res {
                Ok(_) => {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(self.get_out_path(true))
                        .and_then(|mut f| f.write_all(b""))?;
                }
                Err(e) if i == 0 => {
                    return err_jh
                        .join()
                        .unwrap()
                        .and(kak_jh.join().expect("couldn't join kak thread"))
                        .map_err(|_| e);
                }
                Err(_) => {
                    return out_jh
                        .join()
                        .unwrap()
                        .and(err_jh.join().unwrap())
                        .and(kak_jh.join().expect("couldn't join kak thread"));
                }
            }
        }

        Ok(())
    }
}

impl Context<'_> {
    fn get_out_path(&self, err_out: bool) -> PathBuf {
        if err_out {
            self.out_path.with_extension("err")
        } else {
            self.out_path.with_extension("out")
        }
    }
}

fn read_err(
    file_path: PathBuf,
    send_ch: Sender<Result<String, Error>>,
) -> thread::JoinHandle<Result<(), Error>> {
    eprintln!("start read: {}", file_path.display());
    thread::spawn(move || {
        let mut buf = String::new();
        std::fs::OpenOptions::new()
            .read(true)
            .open(&file_path)
            .and_then(|mut f| f.read_to_string(&mut buf))?;
        eprintln!("err read done!");
        send_ch
            .send(Err(Error::KakEvalCatch(buf)))
            .map_err(anyhow::Error::new)?;
        Ok(())
    })
}

fn read_out(
    file_path: PathBuf,
    send_ch: Sender<Result<String, Error>>,
) -> thread::JoinHandle<Result<(), Error>> {
    eprintln!("start read: {}", file_path.display());
    thread::spawn(move || {
        let mut buf = String::new();
        loop {
            std::fs::OpenOptions::new()
                .read(true)
                .open(&file_path)
                .and_then(|mut f| f.read_to_string(&mut buf))?;
            if buf.ends_with(END_TOKEN) {
                buf = buf.trim_end_matches(END_TOKEN).into();
                break;
            }
            eprintln!("out read: {:?}", buf);
        }
        eprintln!("out read done!");
        send_ch.send(Ok(buf)).map_err(anyhow::Error::new)?;
        Ok(())
    })
}
