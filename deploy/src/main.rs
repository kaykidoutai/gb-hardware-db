// SPDX-FileCopyrightText: 2017-2023 Joonas Javanainen <joonas.javanainen@gmail.com>
//
// SPDX-License-Identifier: MIT

use anyhow::{anyhow, Error};
use base64::Engine;
use log::{debug, info};
use md5::{Digest, Md5};
use rayon::prelude::*;
use rusoto_core::Region;
use rusoto_s3::{DeleteObjectRequest, ListObjectsV2Request, PutObjectRequest, S3Client, S3};
use simplelog::{ColorChoice, LevelFilter, TermLogger, TerminalMode};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, BufReader},
    path::{Path, PathBuf},
    str,
    sync::Arc,
};
use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};
use tokio::task::spawn_blocking;
use walkdir::{DirEntry, WalkDir};
use xdg_mime::SharedMimeInfo;

#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalFile {
    absolute_path: PathBuf,
    relative_path: PathBuf,
    key: String,
    len: u64,
    md5: [u8; 16],
}

impl LocalFile {
    fn cache_control(&self) -> &'static str {
        static CC_S: &str = "max-age=3600,public";
        static CC_M: &str = "max-age=86400,public";
        static CC_L: &str = "max-age=1209600,public";
        if self.key.starts_with("static/") {
            if self.key.ends_with(".jpg") {
                CC_L
            } else {
                CC_S
            }
        } else if self.key.starts_with("consoles/") || self.key.starts_with("cartridges/") {
            CC_M
        } else {
            CC_S
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteFile {
    key: String,
    len: u64,
    last_modified: Option<OffsetDateTime>,
    e_tag: Option<[u8; 16]>,
}

fn file_md5(path: &Path) -> Result<[u8; 16], Error> {
    let mut hasher = Md5::new();
    let mut file = BufReader::new(File::open(path)?);
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().into())
}

fn scan_local_file(root: &Path, entry: &DirEntry) -> Result<LocalFile, Error> {
    let metadata = entry.metadata()?;
    let relative_path = entry.path().strip_prefix(root)?.to_owned();
    let key = relative_path
        .to_str()
        .ok_or_else(|| anyhow!("Non-UTF8 filename encountered {:?}", relative_path))?
        .to_owned();
    Ok(LocalFile {
        absolute_path: entry.path().canonicalize()?,
        relative_path,
        key,
        len: metadata.len(),
        md5: file_md5(entry.path())?,
    })
}

fn scan_local_files(root: &Path) -> Result<Vec<LocalFile>, Error> {
    let mut entries = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        entries.push(entry);
    }

    Ok(entries
        .into_par_iter()
        .map(|entry| scan_local_file(root, &entry))
        .collect::<Result<Vec<_>, _>>()?)
}

fn parse_e_tag(e_tag: &str) -> Option<[u8; 16]> {
    let e_tag = e_tag.strip_prefix('"')?.strip_suffix('"')?;
    let mut result = [0; 16];
    for (idx, chunk) in e_tag.as_bytes().chunks(2).enumerate() {
        let byte_str = str::from_utf8(chunk).ok()?;
        let byte = u8::from_str_radix(byte_str, 16).ok()?;
        *(result.get_mut(idx)?) = byte;
    }
    Some(result)
}

async fn scan_remote_files<S: S3>(s3: &S, bucket: &str) -> Result<Vec<RemoteFile>, Error> {
    let mut continuation_token = None;
    let mut result = Vec::new();
    loop {
        let output = s3
            .list_objects_v2(ListObjectsV2Request {
                bucket: bucket.to_owned(),
                continuation_token: continuation_token.clone(),
                ..ListObjectsV2Request::default()
            })
            .await?;
        if let Some(contents) = output.contents {
            for obj in contents {
                if let (Some(key), Some(size)) = (obj.key, obj.size) {
                    result.push(RemoteFile {
                        key,
                        len: size as u64,
                        last_modified: obj
                            .last_modified
                            .and_then(|timestamp| OffsetDateTime::parse(&timestamp, &Rfc3339).ok()),
                        e_tag: obj.e_tag.and_then(|e_tag| parse_e_tag(&e_tag)),
                    });
                }
            }
        }
        continuation_token = output.next_continuation_token;
        if continuation_token.is_none() {
            break;
        }
    }
    Ok(result)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let _ = TermLogger::init(
        LevelFilter::Info,
        simplelog::Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    );

    let build_dir = Path::new("build");
    if !build_dir.exists() {
        return Err(anyhow!("Can't find build directory"));
    }
    info!("Scanning local files...");
    let local_files = spawn_blocking(move || scan_local_files(build_dir)).await??;
    info!("Scanned {} local files", local_files.len());

    let s3 = S3Client::new(Region::EuWest1);
    let bucket = "gbhwdb.gekkio.fi";

    info!("Scanning remote files...");
    let remote_files = scan_remote_files(&s3, bucket).await?;
    info!("Scanned {} remote files", remote_files.len());

    info!("Building deployment plan...");
    let local_index: BTreeMap<&str, &LocalFile> = local_files
        .iter()
        .map(|file| (file.key.as_str(), file))
        .collect();
    let remote_index: BTreeMap<&str, &RemoteFile> = remote_files
        .iter()
        .map(|file| (file.key.as_str(), file))
        .collect();

    let mut to_upload = Vec::new();
    for (key, local_file) in local_index.iter() {
        if let Some(remote_file) = remote_index.get(key) {
            if remote_file.e_tag == Some(local_file.md5) {
                debug!("Skipping local file {}: remote match found", key);
                continue;
            } else {
                debug!("Scheduling local file {}: remote ETag mismatch", key);
            }
        } else {
            debug!("Scheduling local file {}: missing from remote", key);
        }
        to_upload.push(local_file);
    }
    info!("{} files scheduled for upload", to_upload.len());

    let mut to_delete = Vec::new();
    for (key, remote_file) in remote_index.iter() {
        if local_index.contains_key(key) {
            continue;
        }
        if let Some(last_modified) = remote_file.last_modified {
            let elapsed = OffsetDateTime::now_utc() - last_modified;
            if elapsed > Duration::weeks(4) {
                to_delete.push(remote_file);
            }
        }
    }
    info!("{} files scheduled for deletion", to_delete.len());

    let shared_mime_info = Arc::new(spawn_blocking(SharedMimeInfo::new).await?);
    let base64 = base64::engine::general_purpose::STANDARD;

    for local_file in to_upload {
        info!("Uploading {}", local_file.key);
        let body = tokio::fs::read(&local_file.absolute_path).await?;
        let (body, mime_guess) = {
            let shared_mime_info = Arc::clone(&shared_mime_info);
            let absolute_path = local_file.absolute_path.clone();
            spawn_blocking(move || {
                let guess = shared_mime_info
                    .guess_mime_type()
                    .path(absolute_path)
                    .data(&body)
                    .guess();
                (body, guess)
            })
            .await?
        };
        if mime_guess.uncertain() {
            return Err(anyhow!("Failed to guess MIME type for {}", local_file.key));
        }
        s3.put_object(PutObjectRequest {
            bucket: bucket.to_owned(),
            key: local_file.key.clone(),
            body: Some(body.into()),
            content_type: Some(mime_guess.mime_type().essence_str().to_owned()),
            content_md5: Some(base64.encode(local_file.md5)),
            cache_control: Some(local_file.cache_control().to_owned()),
            ..PutObjectRequest::default()
        })
        .await?;
    }

    for remote_file in to_delete {
        info!("Deleting {}", remote_file.key);
        s3.delete_object(DeleteObjectRequest {
            bucket: bucket.to_owned(),
            key: remote_file.key.clone(),
            ..DeleteObjectRequest::default()
        })
        .await?;
    }

    info!("Site deployment complete");
    Ok(())
}
