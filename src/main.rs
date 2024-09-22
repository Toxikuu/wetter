use std::fs;
use std::process;
use libc::geteuid;
use std::env;
use std::process::{Command, exit};
use std::path::{Path, PathBuf};
use reqwest::blocking::get;
use reqwest::header::CONTENT_DISPOSITION;
use std::io::{self, Write, Read, Seek};
use tar::Archive;
use std::fs::File;
use flume::{Sender, Receiver};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use xz2::read::XzDecoder;
use std::error::Error;

fn check_perms() {
    unsafe {
        if geteuid() != 0 {
            println!("Insufficient privileges.");
            process::exit(1);
        }
    }
}

fn detect_filetype<R: Read>(reader: &mut R) -> Result<&'static str, io::Error> {
    let mut buffer = [0; 6];
    reader.read_exact(&mut buffer)?;

    match buffer {
        [0x1F, 0x8B, 0x08, ..] => Ok("gz"),
        [b'B', b'Z', b'h', ..] => Ok("bz2"),
        [0xFD, b'7', b'z', b'X', b'Z', 0x00] => Ok("xz"),
        [b'u', b's', ..] => Ok("tar"),
        [0x1F, 0x9D, 0x00, 0x00, ..] => Ok("z"),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "Unknown file type")),
    }
}

fn download_file(url: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let response = get(url)?.error_for_status()?;
    //println!("{:?}", response.headers());
    let content_disposition = response.headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|cd| cd.to_str().ok());

    let filename = match content_disposition {
        Some(cd) => cd.split(';')
            .find_map(|param| {
                let mut parts = param.trim().splitn(2, '=');
                if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                    if key.eq_ignore_ascii_case("filename") {
                        return Some(value.trim_matches('"').to_string());
                    }
                }
                None
            }),
        None => None,
    }.unwrap_or_else(|| {
        url.split('/').last().unwrap_or("unknown.tar").to_string()
    });

    let dest_dir = Path::new("/tmp/wet");
    fs::create_dir_all(dest_dir)?;

    let file_path = dest_dir.join(filename);
    let mut file = File::create(&file_path)?;
    let content = response.bytes()?;
    file.write_all(&content)?;

    Ok(file_path)
}

fn extract_tar(file_path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let file = File::open(file_path)?;
    let mut reader = io::BufReader::new(file);

    let filetype = detect_filetype(&mut reader)?;
    println!("Detected filetype: {}", filetype);

    reader.seek(io::SeekFrom::Start(0))?; // fixed the xz shit

    let archive_reader: Box<dyn Read> = match filetype {
        "tar" => Box::new(reader),
        "gz"  => Box::new(GzDecoder::new(reader)),
        "bz2" => Box::new(BzDecoder::new(reader)),
        "xz"  => Box::new(XzDecoder::new(reader)),
        _ => return Err("Unsupported file format".into()), // should never happen
    };

    let mut archive = Archive::new(archive_reader);
    let extract_dir = Path::new("/tmp/wet");
    archive.unpack(extract_dir).map_err(|e| format!("Failed to unpack archive: {}", e))?;

    let entries: Vec<_> = fs::read_dir(extract_dir)?.collect();
    println!("Contents of extract dir: {:?}", entries);

    let srcdir = fs::read_dir(extract_dir)?
        .filter_map(Result::ok)
        .find(|entry| entry.path().is_dir())
        .expect("No valid source directory found");

    let tarball_name = file_path.file_name().ok_or("Failed to get tarball name")?;
    let current_dir = std::env::current_dir()?;
    let destination = current_dir.join(tarball_name);
    fs::rename(file_path, destination)?;

    Ok(srcdir.file_name().to_string_lossy().to_string())
}

fn fix_quirks() -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(".")? {
        let entry = entry?;
        let path = entry.path();
        if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
            if filename.contains("?viasf=1") {
                let new_name = filename.replace("?viasf=1", "");
                fs::rename(path, new_name)?;
            }
        }
    }
    Ok(())
}

 fn start_shell(srcdir: &str, sender: Sender<()>) -> io::Result<()> {
    println!("\n\x1b[1;3m [\x1b[31m$\x1b[39m] You are now in a wet shell. Type 'x' once you're done.\x1b[0m\n");

    let shell_status = Command::new("bash")
        .arg("--init-file")
        .arg("/etc/wetenv")
        .current_dir(srcdir)
        .status()?;

    if shell_status.success() {
        let _ = sender.send(());
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    check_perms();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: wetter <url>");
        exit(1);
    }
    let url = &args[1];

    fs::remove_dir_all("/tmp/wet").ok();
    fs::create_dir_all("/tmp/wet")?;

    let file_path = download_file(url)?;
    println!("Downloaded to {:?}", file_path);

    let srcdir = extract_tar(&file_path)?;
    println!("Extracted to /tmp/wet/{}", srcdir);

    fs::rename(format!("/tmp/wet/{}", srcdir), &srcdir)?;

    fix_quirks()?;
    
    let (sender, receiver): (Sender<()>, Receiver<()>) = flume::unbounded();
    start_shell(&srcdir, sender)?;

    receiver.recv().unwrap();
    fs::remove_dir_all(&srcdir)?;
    println!("\n\x1b[1;3m [\x1b[31m$\x1b[39m] You have exited the wet shell.\x1b[0m\n");

    Ok(())
}
